"""Read-only Win32 icon proof. Maps EXE resources as data; never executes the target.

Optional --pid checks icons assigned to that exact diagnostic process's window.
Requires Pillow. Saves native DrawIconEx output, not a screenshot of Explorer/taskbar.
"""
import argparse
import ctypes as C
from ctypes import wintypes as W
import json
import struct
from pathlib import Path
import time
from PIL import Image, ImageDraw

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("exe", type=Path)
parser.add_argument("--pid", type=int)
parser.add_argument("--output", type=Path)
args = parser.parse_args()
exe = args.exe.resolve(strict=True)
output = args.output or exe.parent.parent / ".test-output/icon-proof"
output.mkdir(parents=True, exist_ok=True)
kernel = C.WinDLL("kernel32", use_last_error=True)
user = C.WinDLL("user32", use_last_error=True)
gdi = C.WinDLL("gdi32", use_last_error=True)


def bind(dll, name, result, *arguments):
    method = getattr(dll, name)
    method.restype, method.argtypes = result, list(arguments)
    return method


load_library = bind(kernel, "LoadLibraryExW", W.HMODULE, W.LPCWSTR, W.HANDLE, W.DWORD)
free_library = bind(kernel, "FreeLibrary", W.BOOL, W.HMODULE)
load_icon = bind(user, "LoadImageW", W.HANDLE, W.HINSTANCE, C.c_void_p, W.UINT, C.c_int, C.c_int, W.UINT)
destroy_icon = bind(user, "DestroyIcon", W.BOOL, W.HANDLE)
draw_icon = bind(user, "DrawIconEx", W.BOOL, W.HDC, C.c_int, C.c_int, W.HANDLE, C.c_int, C.c_int, W.UINT, W.HBRUSH, W.UINT)
create_dc = bind(gdi, "CreateCompatibleDC", W.HDC, W.HDC)
delete_dc = bind(gdi, "DeleteDC", W.BOOL, W.HDC)
select_object = bind(gdi, "SelectObject", W.HANDLE, W.HDC, W.HANDLE)
delete_object = bind(gdi, "DeleteObject", W.BOOL, W.HANDLE)
flush = bind(gdi, "GdiFlush", W.BOOL)


class BitmapInfo(C.Structure):
    _fields_ = [("size", W.DWORD), ("width", W.LONG), ("height", W.LONG),
                ("planes", W.WORD), ("bits", W.WORD), ("compression", W.DWORD),
                ("image_size", W.DWORD), ("xppm", W.LONG), ("yppm", W.LONG),
                ("colors_used", W.DWORD), ("colors_important", W.DWORD)]


create_dib = bind(gdi, "CreateDIBSection", W.HBITMAP, W.HDC, C.POINTER(BitmapInfo), W.UINT,
                  C.POINTER(C.c_void_p), W.HANDLE, W.DWORD)


def render(icon, width, height, background):
    dc = create_dc(None)
    assert dc, C.WinError(C.get_last_error())
    info = BitmapInfo(C.sizeof(BitmapInfo), width, -height, 1, 32, 0, width * height * 4, 0, 0, 0, 0)
    bits = C.c_void_p()
    bitmap = create_dib(dc, C.byref(info), 0, C.byref(bits), None, 0)
    assert bitmap and bits.value, C.WinError(C.get_last_error())
    old = select_object(dc, bitmap)
    try:
        red, green, blue = background
        pixels = bytes((blue, green, red, 255)) * (width * height)
        C.memmove(bits, pixels, len(pixels))
        assert draw_icon(dc, 0, 0, icon, width, height, 0, None, 3), C.WinError(C.get_last_error())
        assert flush()
        image = Image.frombytes("RGB", (width, height), C.string_at(bits, len(pixels)), "raw", "BGRX")
        assert len(set(image.tobytes()[::3])) > 1, "Blank or corrupt native icon output"
        return image
    finally:
        select_object(dc, old)
        delete_object(bitmap)
        delete_dc(dc)


sizes = (16, 20, 24, 32, 40, 48, 64, 128, 256)
backgrounds = ((244, 247, 245), (32, 40, 38))
sheet = Image.new("RGB", (1040, 640), backgrounds[0])
draw = ImageDraw.Draw(sheet)
draw.text((16, 10), "Actual EXE resources rendered with Win32 DrawIconEx (native pixels)", fill="#18382e")
positions = (16, 64, 116, 172, 236, 308, 388, 484, 644)
module = load_library(str(exe), None, 0x2 | 0x20)  # DATAFILE | IMAGE_RESOURCE, no target execution
assert module, C.WinError(C.get_last_error())
try:
    find_resource = bind(kernel, "FindResourceW", W.HANDLE, W.HMODULE, C.c_void_p, C.c_void_p)
    load_resource = bind(kernel, "LoadResource", W.HANDLE, W.HMODULE, W.HANDLE)
    lock_resource = bind(kernel, "LockResource", C.c_void_p, W.HANDLE)
    resource_size = bind(kernel, "SizeofResource", W.DWORD, W.HMODULE, W.HANDLE)
    def resource(kind, identifier):
        handle = find_resource(module, identifier, kind)
        assert handle, f"Missing resource {kind}/{identifier}"
        loaded = load_resource(module, handle)
        pointer = lock_resource(loaded)
        assert pointer
        return C.string_at(pointer, resource_size(module,handle))
    group = resource(14,1)
    assert struct.unpack_from("<HHH",group) == (0,1,len(sizes))
    seen = set()
    for index in range(len(sizes)):
        width,height,_,_,planes,depth,length,identifier = struct.unpack_from("<BBBBHHIH",group,6+14*index)
        width,height = width or 256,height or 256
        data = resource(3,identifier)
        assert planes == 1 and depth == 32 and len(data) == length
        assert struct.unpack_from("<III",data) == (40,width,height*2)
        pixels = Image.frombytes("RGBA",(width,height),data[40:40+width*height*4],"raw","BGRA",0,-1)
        bounds = pixels.getchannel("A").getbbox()
        assert bounds and bounds[0] > 0 and bounds[1] > 0 and bounds[2] < width and bounds[3] < height
        assert pixels.getchannel("A").getextrema() == (0,255)
        assert (bounds[2]-bounds[0]) >= width*0.8 and (bounds[3]-bounds[1]) >= height*0.8
        print(f"Embedded {width}x{height}: visible alpha bounds {bounds}")
        seen.add(width)
    assert seen == set(sizes)
    for row, background in enumerate(backgrounds):
        y = 40 + row * 300
        draw.rectangle((0, y, sheet.width, y + 290), fill=background)
        for size, x in zip(sizes, positions):
            icon = load_icon(module, 1, 1, size, size, 0)  # group 1, IMAGE_ICON
            assert icon, f"Resource icon {size}px did not load: {C.WinError(C.get_last_error())}"
            try:
                sheet.paste(render(icon, size, size, background), (x, y + 14))
                draw.text((x, y + size + 18), str(size), fill="#19634f" if row == 0 else "#f4f7f5")
            finally:
                destroy_icon(icon)
    sheet.save(output / "resources.png")
    # Load the same native small-icon metric used by production tray startup.
    controls = C.WinDLL("comctl32", use_last_error=True)
    load_metric = bind(controls, "LoadIconMetric", C.c_long, W.HINSTANCE, C.c_void_p, C.c_int, C.POINTER(W.HANDLE))
    get_metric = bind(user, "GetSystemMetrics", C.c_int, C.c_int)
    tray = W.HANDLE()
    assert load_metric(module,1,0,C.byref(tray)) >= 0 and tray.value
    try:
        metric = (get_metric(49),get_metric(50))  # SM_CXSMICON, SM_CYSMICON
        tray_proof = Image.new("RGB",(180,100),backgrounds[0])
        labels = ImageDraw.Draw(tray_proof)
        labels.text((8,8),f"LoadIconMetric: {metric[0]} x {metric[1]}",fill="#18382e")
        for index, background in enumerate(backgrounds):
            tray_proof.paste(render(tray,*metric,background),(12+index*80,40))
        tray_proof.save(output / "tray-metric.png")
        print(f"Production tray resource loaded with native small-icon metric {metric}")
    finally:
        destroy_icon(tray)

    result = {"exe": str(exe), "resource_sizes": list(sizes), "runtime": None}
    if args.pid is not None:
        enum_callback = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
        enum_windows = bind(user, "EnumWindows", W.BOOL, enum_callback, W.LPARAM)
        window_pid = bind(user, "GetWindowThreadProcessId", W.DWORD, W.HWND, C.POINTER(W.DWORD))
        send = bind(user, "SendMessageTimeoutW", C.c_size_t, W.HWND, W.UINT, C.c_size_t, C.c_ssize_t,
                    W.UINT, W.UINT, C.POINTER(C.c_size_t))
        matches = []

        @enum_callback
        def find(hwnd, _):
            pid = W.DWORD()
            window_pid(hwnd, C.byref(pid))
            if pid.value == args.pid:
                matches.append(hwnd)
            return True

        deadline = time.monotonic() + 3
        assigned = None
        while time.monotonic() < deadline and assigned is None:
            matches.clear()
            enum_windows(find, 0)
            for hwnd in matches:
                icons = []
                for kind in (0, 1):  # ICON_SMALL, ICON_BIG, read-only WM_GETICON
                    value = C.c_size_t()
                    if send(hwnd, 0x007F, kind, 0, 2, 100, C.byref(value)) and value.value:
                        icons.append(value.value)
                if len(icons) == 2:
                    assigned = icons
                    break
            if assigned is None:
                time.sleep(0.025)
        assert assigned, f"PID {args.pid} has no window with both custom icons assigned"

        class IconInfo(C.Structure):
            _fields_ = [("icon", W.BOOL), ("x", W.DWORD), ("y", W.DWORD),
                        ("mask", W.HBITMAP), ("color", W.HBITMAP)]

        class Bitmap(C.Structure):
            _fields_ = [("type", W.LONG), ("width", W.LONG), ("height", W.LONG),
                        ("stride", W.LONG), ("planes", W.WORD), ("bits", W.WORD), ("pixels", C.c_void_p)]

        get_info = bind(user, "GetIconInfo", W.BOOL, W.HANDLE, C.POINTER(IconInfo))
        get_object = bind(gdi, "GetObjectW", C.c_int, W.HANDLE, C.c_int, C.c_void_p)
        runtime = Image.new("RGB", (400, 180), backgrounds[0])
        labels = ImageDraw.Draw(runtime)
        result["runtime"] = {"pid": args.pid, "icons": []}
        for index, icon in enumerate(assigned):
            info, bitmap = IconInfo(), Bitmap()
            assert get_info(icon, C.byref(info))
            try:
                assert info.color and get_object(info.color, C.sizeof(bitmap), C.byref(bitmap))
                width, height = bitmap.width, bitmap.height
            finally:
                if info.color:
                    delete_object(info.color)
                if info.mask:
                    delete_object(info.mask)
            expected = load_icon(module, 1, 1, width, height, 0)
            assert expected
            try:
                actual = render(icon, width, height, backgrounds[0])
                assert actual.tobytes() == render(expected, width, height, backgrounds[0]).tobytes(), "Assigned icon differs from EXE resource"
                x = 20 + index * 190
                runtime.paste(actual, (x, 50))
                name = ("Window (small)", "Taskbar/Alt-Tab (big)")[index]
                labels.text((x, 18), name, fill="#18382e")
                labels.text((x, 100), f"{width} x {height}; resource match", fill="#18382e")
                result["runtime"]["icons"].append({"kind": name, "width": width, "height": height, "matches_resource": True})
            finally:
                destroy_icon(expected)
        runtime.save(output / "runtime.png")
    (output / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(f"Verified native EXE icon output at {sizes}; proof: {output / 'resources.png'}")
    if args.pid is not None:
        print(f"Verified both assigned icons for exact diagnostic PID {args.pid}; proof: {output / 'runtime.png'}")
finally:
    free_library(module)
