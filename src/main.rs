mod db;
mod engine;
mod bot;
mod backup;

use anyhow::Result;
use dotenv::dotenv;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tokio::signal;
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<()> {
    // 환경 변수 로드
    dotenv().ok();
    println!("Starting Genesis Miner...");

    // DB 초기화 및 마이그레이션 실행
    let db = Arc::new(db::Database::new().await?);
    println!("Database initialized successfully.");

    // 엔진 초기화
    let engine = Arc::new(engine::MiningEngine::new()?);
    println!("MiningEngine initialized successfully.");
    
    // 백업 매니저 초기화
    let backuper = Arc::new(backup::BackupManager::new()?);
    println!("BackupManager initialized successfully.");

    // 봇 백그라운드 실행
    let bot_db = db.clone();
    let bot_engine = engine.clone();
    let bot_backuper = backuper.clone();
    
    // 봇 루프를 백그라운드 태스크로 분리
    let _bot_handle = tokio::spawn(async move {
        if let Err(e) = bot::run_bot(bot_db, bot_engine, bot_backuper).await {
            eprintln!("Bot task failed: {}", e);
        }
    });

    println!("All systems go. Press Ctrl+C to exit.");

    // Graceful shutdown channel
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    
    // Ctrl+C 감지 태스크
    tokio::spawn(async move {
        signal::ctrl_c().await.expect("Failed to listen for event");
        println!("\n[Shutdown] Ctrl+C received, initiating graceful shutdown...");
        let _ = shutdown_tx.send(()).await;
    });

    // 메인 스케줄러 루프
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                println!("[Shutdown] Stopping scheduler...");
                break;
            }
            _ = sleep(Duration::from_secs(60)) => {
                // 매 1분마다 실행
                match db.get_keywords().await {
                    Ok(keywords) => {
                        let active_keywords = keywords.into_iter().filter(|k| k.is_active);
                        for kw in active_keywords {
                            let should_run = if let Some(last) = kw.last_run {
                                let interval = kw.interval_min.unwrap_or(1440);
                                let elapsed = Utc::now().signed_duration_since(last).num_minutes();
                                elapsed >= interval
                            } else {
                                true // 한 번도 실행되지 않음
                            };

                            if should_run {
                                println!("[Scheduler] Auto-mining triggered for: {}", kw.word);
                                let sched_engine = engine.clone();
                                let sched_db = db.clone();
                                let kw_clone = kw;
                                
                                // 개별 키워드 채굴을 백그라운드 태스크로 던짐
                                tokio::spawn(async move {
                                    if let Err(e) = sched_engine.run_mining(&kw_clone, &sched_db).await {
                                        eprintln!("[Scheduler] Auto-mining failed for {}: {}", kw_clone.word, e);
                                    } else {
                                        println!("[Scheduler] Auto-mining finished for: {}", kw_clone.word);
                                    }
                                });
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[Scheduler] Failed to fetch keywords: {}", e);
                    }
                }
            }
        }
    }

    println!("Shutting down complete. Goodbye!");
    Ok(())
}
