use super::{Failure, oauth, replica::Replica};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";
pub const MAX_CLOUD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CloudFile {
    pub id: String,
    #[serde(rename = "md5Checksum")]
    pub checksum: Option<String>,
}
#[derive(Clone, Deserialize)]
pub struct Account {
    #[serde(rename = "permissionId")]
    pub id: String,
    #[serde(rename = "emailAddress", default)]
    pub email: String,
    #[serde(rename = "displayName", default)]
    pub name: String,
}

pub trait Backend {
    fn list(&self) -> Result<Vec<CloudFile>, Failure>;
    fn download(&self, id: &str) -> Result<Replica, Failure>;
    fn generate_id(&self) -> Result<String, Failure>;
    fn exists(&self, id: &str) -> Result<bool, Failure>;
    fn upload(
        &self,
        id: &str,
        device: Uuid,
        data: &Replica,
        create: bool,
    ) -> Result<CloudFile, Failure>;
}

pub struct Google {
    api: String,
    upload: String,
    access: String,
    cancel: Option<Arc<AtomicBool>>,
}
impl Google {
    pub fn new(access: String) -> Self {
        Self {
            api: API.into(),
            upload: UPLOAD.into(),
            access,
            cancel: None,
        }
    }
    pub fn cancellable(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }
    fn check_cancelled(&self) -> Result<(), Failure> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::SeqCst))
        {
            Err(Failure::data("Google Drive synchronization was cancelled."))
        } else {
            Ok(())
        }
    }
    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.api)
    }
    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, Failure> {
        self.check_cancelled()?;
        oauth::http().get(url).header("Authorization", &format!("Bearer {}", self.access)).call().map_err(|_| Failure::retry("Cannot contact Google Drive. Working offline; synchronization will retry automatically."))
    }
    pub fn account(&self) -> Result<Account, Failure> {
        let mut response =
            self.get(&self.endpoint("/about?fields=user(permissionId,emailAddress,displayName)"))?;
        checked(&response)?;
        #[derive(Deserialize)]
        struct About {
            user: Account,
        }
        let about: About = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_json()
            .map_err(|_| Failure::data("Google Drive returned an invalid account response."))?;
        if about.user.id.is_empty() || about.user.id.len() > 256 {
            return Err(Failure::data(
                "Google Drive returned no stable account identifier.",
            ));
        }
        Ok(about.user)
    }
}
fn checked(response: &ureq::http::Response<ureq::Body>) -> Result<(), Failure> {
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(());
    }
    Err(match status {
        401 => Failure::access_expired(
            "The Google access token expired. The session will be refreshed before retrying.",
        ),
        403 => Failure::retry(
            "Google Drive refused access. Check that Drive API is enabled, app-data permission was granted, and the account has available storage or API quota.",
        ),
        404 => Failure::retry(
            "A Google Drive sync file is temporarily unavailable. Local data is preserved; retry synchronization.",
        ),
        409 => Failure::retry(
            "Google Drive already created this sync file. Retrying will discover the existing file.",
        ),
        429 => Failure::retry(
            "Google Drive rate limit reached. Synchronization will retry with backoff.",
        ),
        500..=599 => Failure::retry(
            "Google Drive is temporarily unavailable. Local changes will be synchronized later.",
        ),
        _ => Failure::data(format!(
            "Google Drive returned HTTP {status}. Local data was not discarded."
        )),
    })
}
fn file_url(root: &str, id: &str) -> Result<String, Failure> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Failure::data(
            "Google Drive returned an invalid file identifier.",
        ));
    }
    Ok(format!("{root}/files/{id}"))
}
impl Backend for Google {
    fn list(&self) -> Result<Vec<CloudFile>, Failure> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<CloudFile>,
            #[serde(rename = "nextPageToken")]
            next: Option<String>,
        }
        let mut result = vec![];
        let mut page = None::<String>;
        let mut seen = BTreeSet::new();
        loop {
            let mut url = url::Url::parse(&self.endpoint("/files")).unwrap();
            url.query_pairs_mut().extend_pairs([("spaces", "appDataFolder"), ("q", "trashed = false and appProperties has { key='subscriptions_reminder' and value='sync-v1' }"), ("fields", "nextPageToken,files(id,md5Checksum)"), ("pageSize", "1000")]);
            if let Some(token) = &page {
                url.query_pairs_mut().append_pair("pageToken", token);
            }
            let mut response = self.get(url.as_str())?;
            checked(&response)?;
            let data: Page = response
                .body_mut()
                .with_config()
                .limit(2 * 1024 * 1024)
                .read_json()
                .map_err(|_| Failure::data("Google Drive returned an invalid sync-file list."))?;
            result.extend(data.files);
            if result.len() > 10_000 {
                return Err(Failure::data(
                    "Too many synchronization snapshots exist in Google Drive.",
                ));
            }
            match data.next.filter(|token| !token.is_empty()) {
                Some(token) => {
                    if !seen.insert(token.clone()) {
                        return Err(Failure::data(
                            "Google Drive repeated a page token. Synchronization was stopped without overwriting data.",
                        ));
                    }
                    page = Some(token);
                }
                None => break,
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result.dedup_by(|a, b| a.id == b.id);
        Ok(result)
    }
    fn download(&self, id: &str) -> Result<Replica, Failure> {
        let mut response = self.get(&format!("{}?alt=media", file_url(&self.api, id)?))?;
        checked(&response)?;
        let data: Replica = response.body_mut().with_config().limit(MAX_CLOUD_BYTES).read_json().map_err(|_| Failure::data("A Google Drive sync file is invalid or exceeds 8 MB. Local subscriptions were not overwritten."))?;
        data.validate().map_err(Failure::data)?;
        Ok(data)
    }
    fn generate_id(&self) -> Result<String, Failure> {
        #[derive(Deserialize)]
        struct Ids {
            ids: Vec<String>,
        }
        let mut response =
            self.get(&self.endpoint("/files/generateIds?count=1&space=appDataFolder&type=files"))?;
        checked(&response)?;
        let ids: Ids = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_json()
            .map_err(|_| Failure::data("Google Drive did not generate a file identifier."))?;
        let id = ids
            .ids
            .first()
            .ok_or_else(|| Failure::data("Google Drive did not generate a file identifier."))?
            .clone();
        file_url(&self.api, &id)?;
        Ok(id)
    }
    fn exists(&self, id: &str) -> Result<bool, Failure> {
        let response = self.get(&format!("{}?fields=id", file_url(&self.api, id)?))?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        checked(&response)?;
        Ok(true)
    }
    fn upload(
        &self,
        id: &str,
        device: Uuid,
        data: &Replica,
        create: bool,
    ) -> Result<CloudFile, Failure> {
        self.check_cancelled()?;
        data.validate().map_err(Failure::data)?;
        let bytes = serde_json::to_vec(data)
            .map_err(|_| Failure::data("Cannot encode the sync database."))?;
        if bytes.len() as u64 > MAX_CLOUD_BYTES {
            return Err(Failure::data(
                "The sync database exceeds the 8 MB limit. Your local data is preserved.",
            ));
        }
        let agent = oauth::http();
        let authorization = format!("Bearer {}", self.access);
        let mut response = if create {
            file_url(&self.api, id)?;
            let metadata = serde_json::json!({"id":id,"name":format!("subscriptions-reminder-{device}.json"),"mimeType":"application/json","parents":["appDataFolder"],"appProperties":{"subscriptions_reminder":"sync-v1","device":device.to_string()}});
            let boundary = format!("sr_{}", Uuid::new_v4().simple());
            let mut body = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/json\r\n\r\n").into_bytes();
            body.extend(bytes); body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
            agent.post(format!("{}/files?uploadType=multipart&fields=id,md5Checksum", self.upload)).header("Authorization", &authorization).header("Content-Type", &format!("multipart/related; boundary={boundary}")).send(body)
        } else {
            agent.patch(format!("{}?uploadType=media&fields=id,md5Checksum", file_url(&self.upload, id)?)).header("Authorization", &authorization).header("Content-Type", "application/json").send(bytes)
        }.map_err(|_| Failure::retry("Google Drive upload was interrupted. Local changes are safe and the upload will be retried."))?;
        checked(&response)?;
        let file: CloudFile = response.body_mut().with_config().limit(64 * 1024).read_json().map_err(|_| Failure::retry("The upload response could not be confirmed. The next sync will check Google Drive before retrying."))?;
        if file.id != id {
            return Err(Failure::data(
                "Google Drive returned a different file identifier. Synchronization was stopped.",
            ));
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::test_support::server;
    use serde_json::json;
    #[test]
    fn drive_transport_paginates_downloads_and_uses_idempotent_scoped_uploads() {
        let replica = Replica::default();
        let device = Uuid::new_v4();
        let (root, worker) = server(vec![
            (
                200,
                json!({"files":[{"id":"remote1","md5Checksum":"a"}],"nextPageToken":"second"}),
            ),
            (200, json!({"files":[{"id":"remote2","md5Checksum":"b"}]})),
            (200, serde_json::to_value(&replica).unwrap()),
            (200, json!({"ids":["local1"]})),
            (404, json!({"error":{"code":404}})),
            (200, json!({"id":"local1","md5Checksum":"created"})),
            (200, json!({"id":"local1","md5Checksum":"updated"})),
        ]);
        let backend = Google {
            api: format!("{root}/drive/v3"),
            upload: format!("{root}/upload/drive/v3"),
            access: "test-access-token".into(),
            cancel: None,
        };
        assert_eq!(backend.list().unwrap().len(), 2);
        assert_eq!(backend.download("remote1").unwrap(), replica);
        assert_eq!(backend.generate_id().unwrap(), "local1");
        assert!(!backend.exists("local1").unwrap());
        backend.upload("local1", device, &replica, true).unwrap();
        backend.upload("local1", device, &replica, false).unwrap();
        let requests = worker.join().unwrap();
        assert!(requests[0].target.contains("appDataFolder"));
        assert!(requests[1].target.contains("pageToken=second"));
        assert!(requests[2].target.contains("/files/remote1?alt=media"));
        assert!(requests.iter().all(|r| {
            r.headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-access-token")
        }));
        let multipart = String::from_utf8(requests[5].body.clone()).unwrap();
        assert!(multipart.contains("\"id\":\"local1\""));
        assert!(multipart.contains("\"parents\":[\"appDataFolder\"]"));
        assert!(!multipart.contains("test-access-token"));
        assert!(!multipart.contains("refresh_token"));
        assert!(
            requests[6]
                .headers
                .starts_with("PATCH /upload/drive/v3/files/local1?uploadType=media")
        );
        assert_eq!(
            serde_json::from_slice::<Replica>(&requests[6].body).unwrap(),
            replica
        );
    }
    #[test]
    fn malformed_cloud_data_and_repeated_pages_are_rejected() {
        let (root, worker) = server(vec![(200, json!({"format_version":99,"records":{}}))]);
        let backend = Google {
            api: root.clone(),
            upload: root,
            access: "test".into(),
            cancel: None,
        };
        assert!(backend.download("file1").is_err());
        worker.join().unwrap();
        let (root, worker) = server(vec![
            (200, json!({"files":[],"nextPageToken":"same"})),
            (200, json!({"files":[],"nextPageToken":"same"})),
        ]);
        let backend = Google {
            api: root.clone(),
            upload: root,
            access: "test".into(),
            cancel: None,
        };
        assert!(backend.list().unwrap_err().message.contains("repeated"));
        worker.join().unwrap();
    }
    #[test]
    fn unauthorized_drive_responses_request_an_access_token_refresh() {
        let (root, worker) = server(vec![(401, json!({"error":{"message":"not displayed"}}))]);
        let backend = Google {
            api: root.clone(),
            upload: root,
            access: "test".into(),
            cancel: None,
        };
        assert!(backend.list().unwrap_err().access_expired);
        worker.join().unwrap();
    }
}
