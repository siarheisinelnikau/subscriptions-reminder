"""Finite isolated production-renderer diagnostic; no OS input or computer use."""
import json
import os
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
exe = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else root / "target/release/subscriptions-reminder.exe"
fixture = root / ".qa/rendering-gate"
fixture.mkdir(parents=True, exist_ok=True)
report = fixture / "report.json"
if report.exists(): report.unlink()
process = subprocess.Popen([str(exe), "--demo", "--data-dir", str(fixture / f"data-{os.getpid()}"),
    "--diagnostics", str(report)], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
try:
    subprocess.run([sys.executable, str(root / "scripts/Check-Icons.py"), str(exe),
        "--pid", str(process.pid), "--output", str(fixture / "icons")], cwd=root, check=True, timeout=5)
    stdout, stderr = process.communicate(timeout=25)
    if process.returncode: raise RuntimeError((stdout + stderr).decode(errors="replace"))
except BaseException:
    if process.poll() is not None:
        stdout, stderr = process.communicate()
        print(f"Diagnostic process exit code: {process.returncode}; {(stdout + stderr).decode(errors='replace')}", file=sys.stderr)
    raise
finally:
    if process.poll() is None:
        process.terminate()
        process.communicate(timeout=3)
result = json.loads(report.read_text())
assert "CPU" in result["backend"] and "GDI" in result["backend"]
assert result["loaded_graphics_modules"] == ["gdi32.dll"], result["loaded_graphics_modules"]
samples = {sample["state"]: sample for sample in result["samples"]}
assert samples["minimized_end"]["window_minimized"] is True
assert samples["restored_settled"]["window_minimized"] is False
assert samples["hidden_end"]["window_visible"] is False
assert samples["minimized_end"]["counters"]["hidden_wakes"] > samples["visible_idle_end"]["counters"]["hidden_wakes"]
assert samples["hidden_end"]["counters"]["hidden_wakes"] > samples["restored_idle_end"]["counters"]["hidden_wakes"]
assert samples["restored_settled"]["counters"]["presents"] > samples["minimized_end"]["counters"]["presents"]
for label, start, end in [("Visible idle","settled_start","visible_idle_end"),
    ("Minimized","visible_idle_end","minimized_end"),
    ("Restored idle","restored_settled","restored_idle_end"),
    ("Hidden","restored_idle_end","hidden_end")]:
    before, after = samples[start], samples[end]
    duration = after["elapsed_seconds"] - before["elapsed_seconds"]
    assert before["cpu_seconds"] >= 0 and after["cpu_seconds"] >= 0
    cpu = 100 * (after["cpu_seconds"] - before["cpu_seconds"]) / duration
    delta = {name:after["counters"][name]-before["counters"][name]
        for name in ("layouts","rasters","presents","cached_blits")}
    assert delta["layouts"] == 0 and delta["rasters"] == 0, (label, delta)
    assert delta["presents"] == delta["cached_blits"], (label, delta)
    if label in ("Minimized","Hidden"): assert delta["presents"] == 0, (label,delta)
    assert cpu < 5, (label,cpu)
    print(f"{label}: {duration:.2f}s, CPU {cpu:.3f}% of one core ({cpu/result['logical_cores']:.4f}% machine), {delta}")
print("Backend:", result["backend"], "modules:", result["loaded_graphics_modules"])
print("Diagnostic exited. Input latency, real tray interaction and compositor/GPU telemetry were not measured.")
