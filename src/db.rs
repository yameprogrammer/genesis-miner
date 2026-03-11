use rusqlite::{params, Connection};
use anyhow::{Context, Result};
use std::env;
use std::path::Path;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

// Keyword Struct
#[derive(Debug)]
pub struct Keyword {
    pub id: i64,
    pub word: String,
    pub category: Option<String>,
    pub interval_min: Option<i64>,
    pub last_run: Option<DateTime<Utc>>,
    pub is_active: bool,
}

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// 데이터베이스 연결 및 초기화 (마이그레이션 수행)
    pub async fn new() -> Result<Self> {
        let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "./miner_main.db".to_string());
        
        // Ensure directory exists
        if let Some(parent) = Path::new(&db_url).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).context("데이터베이스 디렉토리를 생성하는 데 실패했습니다.")?;
            }
        }

        let conn = Connection::open(&db_url).context("SQLite 데이터베이스를 여는 데 실패했습니다.")?;
        
        let db = Database { conn: Mutex::new(conn) };
        db.run_migrations().await?;
        
        Ok(db)
    }

    /// 스키마 마이그레이션 실행 로직
    async fn run_migrations(&self) -> Result<()> {
        let user_version: i64 = {
            let conn = self.conn.lock().await;

            // 스키마 버전 기록용 테이블 (schema_migrations)
            conn.execute(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
                )",
                [],
            ).context("schema_migrations 테이블 생성에 실패했습니다.")?;

            conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            ).unwrap_or(0)
        };

        // v1: Initial Schema
        if user_version < 1 {
            let conn = self.conn.lock().await;
            conn.execute_batch(
                "BEGIN;
                CREATE TABLE IF NOT EXISTS keywords (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    word TEXT NOT NULL UNIQUE,
                    category TEXT,
                    interval_min INTEGER,
                    last_run DATETIME,
                    is_active BOOLEAN DEFAULT 1
                );
                
                CREATE TABLE IF NOT EXISTS mined_data (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    keyword_id INTEGER,
                    title TEXT NOT NULL,
                    content TEXT,
                    source_url TEXT,
                    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY(keyword_id) REFERENCES keywords(id) ON DELETE CASCADE
                );
                
                CREATE TABLE IF NOT EXISTS backup_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    file_name TEXT NOT NULL,
                    cloud_provider TEXT,
                    status TEXT,
                    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
                );
                
                INSERT INTO schema_migrations (version) VALUES (1);
                COMMIT;
                "
            ).context("v1 스키마 마이그레이션 적용에 실패했습니다.")?;
        }

        // v2: Add is_full_text flag
        if user_version < 2 {
            let conn = self.conn.lock().await;
            conn.execute_batch(
                "BEGIN;
                ALTER TABLE mined_data ADD COLUMN is_full_text BOOLEAN DEFAULT 0;
                INSERT INTO schema_migrations (version) VALUES (2);
                COMMIT;
                "
            ).context("v2 스키마 마이그레이션 적용에 실패했습니다.")?;
        }

        Ok(())
    }

    // ==========================================
    // CRUD Interface for Keywords
    // ==========================================

    /// 키워드 추가
    pub async fn add_keyword(&self, word: &str, category: Option<&str>, interval_min: Option<i64>) -> Result<()> {
        let res = {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO keywords (word, category, interval_min) VALUES (?1, ?2, ?3)",
                params![word, category, interval_min],
            )
        };
        res.context("키워드 추가에 실패했습니다.")?;
        Ok(())
    }

    /// 키워드 조회
    pub async fn get_keywords(&self) -> Result<Vec<Keyword>> {
        let keywords = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare("SELECT id, word, category, interval_min, last_run, is_active FROM keywords")?;
            let keyword_iter = stmt.query_map([], |row| {
                Ok(Keyword {
                    id: row.get(0)?,
                    word: row.get(1)?,
                    category: row.get(2)?,
                    interval_min: row.get(3)?,
                    last_run: row.get(4)?,
                    is_active: row.get(5)?,
                })
            })?;

            let mut kws = Vec::new();
            for k in keyword_iter {
                kws.push(k?);
            }
            kws
        };
        Ok(keywords)
    }

    /// 키워드 삭제
    pub async fn remove_keyword(&self, id: i64) -> Result<()> {
        let res = {
            let conn = self.conn.lock().await;
            conn.execute(
                "DELETE FROM keywords WHERE id = ?1",
                params![id],
            )
        };
        res.context("키워드 삭제에 실패했습니다.")?;
        Ok(())
    }

    /// 마지막 실행 시간 업데이트
    pub async fn update_last_run(&self, id: i64) -> Result<()> {
        let now = Utc::now();
        let res = {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE keywords SET last_run = ?1 WHERE id = ?2",
                params![now, id],
            )
        };
        res.context("마지막 실행 시간 업데이트에 실패했습니다.")?;
        Ok(())
    }

    /// 오늘 수집된 데이터 개수 조회
    pub async fn get_today_mined_count(&self) -> Result<i64> {
        let count: i64 = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT COUNT(*) FROM mined_data WHERE date(created_at) = date('now')",
                [],
                |row| row.get(0),
            ).context("오늘 수집된 데이터 개수 조회에 실패했습니다.")?
        };
        Ok(count)
    }

    /// 수집된 데이터(민데이터) 추가
    pub async fn add_mined_data(&self, keyword_id: i64, title: &str, content: &str, source_url: &str, is_full_text: bool) -> Result<()> {
        let res = {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO mined_data (keyword_id, title, content, source_url, is_full_text) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![keyword_id, title, content, source_url, is_full_text],
            )
        };
        res.context("Mined data 추가에 실패했습니다.")?;
        Ok(())
    }

    /// 최근 수집된 데이터 조회
    pub async fn get_recent_mined_data(&self, limit: i64) -> Result<Vec<(i64, String, bool)>> {
        let results = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare("SELECT id, title, is_full_text FROM mined_data ORDER BY created_at DESC LIMIT ?1")?;
            let data_iter = stmt.query_map([limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;

            let mut res = Vec::new();
            for items in data_iter {
                res.push(items?);
            }
            res
        };
        Ok(results)
    }

    /// ID로 수집된 상세 데이터 조회
    pub async fn get_mined_data_by_id(&self, id: i64) -> Result<(String, String, String, bool)> {
        let result: (String, String, String, bool) = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT title, content, source_url, is_full_text FROM mined_data WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).context(format!("ID {}에 해당하는 데이터를 찾을 수 없습니다.", id))?
        };
        Ok(result)
    }
}
