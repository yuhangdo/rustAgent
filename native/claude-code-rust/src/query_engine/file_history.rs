use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshotRecord {
    pub snapshot_id: String,
    pub session_id: String,
    pub event_id: String,
    pub path: String,
    pub snapshot_rel_path: String,
    pub existed_before: bool,
    pub order_index: usize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct FileHistoryStore {
    root: PathBuf,
}

impl FileHistoryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn session_root(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    fn snapshot_root(&self, session_id: &str) -> PathBuf {
        self.session_root(session_id).join("snapshots")
    }

    fn index_path(&self, session_id: &str) -> PathBuf {
        self.session_root(session_id).join("index.jsonl")
    }

    pub async fn snapshot(&self, session_id: &str, event_id: &str, path: &Path) -> Result<String> {
        tokio::fs::create_dir_all(self.snapshot_root(session_id)).await?;
        let existing = self.read_records(session_id).await?;
        let order_index = existing.len();
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let snapshot_file = format!("{}.snapshot", snapshot_id);
        let snapshot_path = self.snapshot_root(session_id).join(&snapshot_file);
        let existed_before = path.exists();
        let contents = if existed_before {
            tokio::fs::read(path).await?
        } else {
            Vec::new()
        };
        tokio::fs::write(&snapshot_path, contents).await?;

        let record = FileSnapshotRecord {
            snapshot_id: snapshot_id.clone(),
            session_id: session_id.to_string(),
            event_id: event_id.to_string(),
            path: path.display().to_string(),
            snapshot_rel_path: format!("snapshots/{}", snapshot_file),
            existed_before,
            order_index,
            created_at: Utc::now(),
        };

        let serialized = serde_json::to_string(&record)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path(session_id))
            .await?;
        file.write_all(serialized.as_bytes()).await?;
        file.write_all(b"\n").await?;

        Ok(snapshot_id)
    }

    pub async fn rewind_to_event(&self, session_id: &str, event_id: &str) -> Result<Vec<String>> {
        let records = self.read_records(session_id).await?;
        let Some(start_index) = records
            .iter()
            .position(|record| record.event_id == event_id)
        else {
            return Err(anyhow!("Rewind target event not found: {}", event_id));
        };

        let mut restored_paths = Vec::new();
        let mut seen_paths = HashSet::new();
        for record in records.into_iter().skip(start_index) {
            if !seen_paths.insert(record.path.clone()) {
                continue;
            }

            let snapshot_path = self
                .session_root(session_id)
                .join(&record.snapshot_rel_path);
            if !snapshot_path.exists() {
                return Err(anyhow!(
                    "Missing snapshot content: {}",
                    snapshot_path.display()
                ));
            }

            if record.existed_before {
                if let Some(parent) = Path::new(&record.path).parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                let bytes = tokio::fs::read(&snapshot_path).await?;
                tokio::fs::write(&record.path, bytes).await?;
            } else if Path::new(&record.path).exists() {
                tokio::fs::remove_file(&record.path).await?;
            }
            restored_paths.push(record.path);
        }

        Ok(restored_paths)
    }

    pub async fn read_records(&self, session_id: &str) -> Result<Vec<FileSnapshotRecord>> {
        let index_path = self.index_path(session_id);
        if !index_path.exists() {
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(index_path).await?;
        let mut records = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record: FileSnapshotRecord = serde_json::from_str(trimmed).map_err(|error| {
                anyhow!("Failed to parse file history line {}: {}", index + 1, error)
            })?;
            records.push(record);
        }
        records.sort_by_key(|record| record.order_index);
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::FileHistoryStore;

    #[tokio::test]
    async fn rewind_restores_file_contents_for_prior_event_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        tokio::fs::write(&file, "before").await.unwrap();

        let history = FileHistoryStore::new(temp.path().join("file-history"));
        history
            .snapshot("session-1", "event-1", &file)
            .await
            .unwrap();
        tokio::fs::write(&file, "after").await.unwrap();

        history
            .rewind_to_event("session-1", "event-1")
            .await
            .unwrap();

        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "before");
    }

    #[tokio::test]
    async fn rewind_deletes_new_file_when_snapshot_was_missing() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("created.txt");

        let history = FileHistoryStore::new(temp.path().join("file-history"));
        history
            .snapshot("session-1", "event-1", &file)
            .await
            .unwrap();
        tokio::fs::write(&file, "after").await.unwrap();

        history
            .rewind_to_event("session-1", "event-1")
            .await
            .unwrap();

        assert!(!file.exists());
    }
}
