use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Path;
use tokio::process::Command;
use chrono::Utc;

pub struct BackupManager {
    backup_dir: String,
}

impl BackupManager {
    pub fn new() -> Result<Self> {
        let backup_dir = env::var("BACKUP_DIR").unwrap_or_else(|_| "./data/backups".to_string());
        
        if !Path::new(&backup_dir).exists() {
            fs::create_dir_all(&backup_dir).context("Backup 디렉토리를 생성하는 데 실패했습니다.")?;
        }

        Ok(Self { backup_dir })
    }

    /// 데이터 압축 후 Rclone을 통한 업로드 및 자동 정리 수행
    pub async fn run_backup(&self) -> Result<String> {
        let now = Utc::now();
        let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
        let archive_name = format!("genesis_miner_{}.tar.gz", timestamp);
        let archive_path = Path::new(&self.backup_dir).join(&archive_name);

        println!("[Backup] Starting backup process: {}", archive_name);

        // 1. 압축 (tar.gz) - data/db 와 data/exports 대상
        // 경로 문제 방지를 위해 CWD를 프로젝트 루트로 가정 (또는 data 폴더 상위)
        let tar_status = Command::new("tar")
            .arg("-czf")
            .arg(&archive_path)
            .arg("./data/db")
            .arg("./data/exports")
            .status()
            .await
            .context("tar 명령어를 실행하지 못했습니다.")?;

        if !tar_status.success() {
            anyhow::bail!("tar 압축에 실패했습니다. (Exit code: {})", tar_status);
        }

        println!("[Backup] Compression successful.");

        // 2. Cloud Sync (Rclone)
        // 설정은 유저가 미리 구성했다고 가정 (remote name: gdrive, path: backups)
        let rclone_args = [
            "copy",
            archive_path.to_str().unwrap(),
            "gdrive:backups/genesis-miner"
        ];
        
        println!("[Backup] Uploading to Google Drive via rclone...");

        let rclone_status = Command::new("rclone")
            .args(&rclone_args)
            .status()
            .await
            .context("rclone 명령어를 실행하지 못했습니다. 시스템에 rclone이 설치되어 있는지 확인하세요.")?;

        if !rclone_status.success() {
            anyhow::bail!("rclone 업로드에 실패했습니다. (Exit code: {})", rclone_status);
        }

        println!("[Backup] Upload successful.");

        // 3. Auto-Cleanup
        if archive_path.exists() {
            fs::remove_file(&archive_path).context("임시 압축 파일 삭제에 실패했습니다.")?;
            println!("[Backup] Local archive cleaned up.");
        }

        Ok(archive_name)
    }
}
