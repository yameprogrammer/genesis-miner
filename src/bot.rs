use anyhow::Result;
use std::env;
use std::sync::Arc;
use teloxide::{prelude::*, utils::command::BotCommands};

use crate::db::Database;
use crate::engine::MiningEngine;
use crate::backup::BackupManager;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "Start the bot")]
    Start,
    #[command(description = "Add a keyword. Usage: /add <keyword> [category]")]
    Add(String),
    #[command(description = "List all registered keywords")]
    List,
    #[command(description = "Manually trigger mining for a keyword. Usage: /mine <keyword>")]
    Mine(String),
    #[command(description = "Remove a keyword. Usage: /remove <keyword>")]
    Remove(String),
    #[command(description = "Trigger cloud backup. Usage: /backup")]
    Backup,
    #[command(description = "Show system status")]
    Status,
    #[command(description = "List recent mined data. Usage: /recent [limit]")]
    Recent(String),
    #[command(description = "Show detailed info of mined data. Usage: /show <id>")]
    Show(i64),
    #[command(description = "Show full mined content. Usage: /read <id>")]
    Read(i64),
    #[command(description = "Download recent markdown exports as a zip file. Usage: /get_files", rename = "get_files")]
    GetFiles,
    #[command(description = "Show this help message")]
    Help,
}

pub struct BotApp {
    pub db: Arc<Database>,
    pub engine: Arc<MiningEngine>,
    pub backuper: Arc<BackupManager>,
    pub admin_id: ChatId,
}

pub async fn run_bot(db: Arc<Database>, engine: Arc<MiningEngine>, backuper: Arc<BackupManager>) -> Result<()> {
    // ADMIN_CHAT_ID 검증
    let admin_chat_str = env::var("ADMIN_CHAT_ID").expect("ADMIN_CHAT_ID must be set in .env");
    let admin_chat_id = ChatId(admin_chat_str.parse::<i64>().expect("ADMIN_CHAT_ID must be an integer"));

    let bot_token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN must be set in .env");
    let bot = Bot::new(bot_token);
    
    let app_state = Arc::new(BotApp {
        db,
        engine,
        backuper,
        admin_id: admin_chat_id,
    });

    println!("[Bot] Telegram Bot is starting in stealth/admin-only mode...");

    // 명령어 자동 등록
    bot.set_my_commands(Command::bot_commands()).await?;

    // 스레드에 묶어서 안전하게 돌리기 체인 구성
    let handler = Update::filter_message()
        .filter(move |msg: Message, state: Arc<BotApp>| {
            // Access Control: 관리자 ID만 허용
            msg.chat.id == state.admin_id
        })
        .filter_command::<Command>()
        .endpoint(command_handler);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![app_state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn command_handler(bot: Bot, msg: Message, cmd: Command, state: Arc<BotApp>) -> ResponseResult<()> {
    use teloxide::types::ParseMode::MarkdownV2;
    use teloxide::utils::markdown::escape;

    println!("Command received: {:?}", cmd);
    let _bot_clone = bot.clone();
    let chat_id = msg.chat.id;

    match cmd {
        Command::Start | Command::Help => {
            bot.send_message(msg.chat.id, escape(&Command::descriptions().to_string())).parse_mode(MarkdownV2).await?;
        }
        Command::Add(args) => {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.is_empty() {
                bot.send_message(msg.chat.id, escape("Usage: /add <keyword> [category] [interval_min]")).parse_mode(MarkdownV2).await?;
                return Ok(());
            }

            let word = parts[0];
            let mut category = None;
            let mut final_interval: i32 = 1440;
            let mut parse_msg = String::new();

            if parts.len() == 2 {
                if let Ok(val) = parts[1].parse::<i32>() {
                    final_interval = val;
                } else {
                    category = Some(parts[1]);
                }
            } else if parts.len() >= 3 {
                category = Some(parts[1]);
                if let Ok(val) = parts[2].parse::<i32>() {
                    final_interval = val;
                } else {
                    println!("[Add] Interval parsing failed for '{}'", parts[2]);
                    parse_msg = format!(" \n⚠️ 주기 파싱 실패: {} \\(기본값 1440분 적용\\)", escape(parts[2]));
                }
            }

            match state.db.add_keyword(word, category, Some(final_interval as i64)).await {
                Ok(_) => {
                    let success_msg = format!("✅ 키워드 추가 완료: {} \\(주기: {}분\\){}", escape(word), final_interval, parse_msg);
                    bot.send_message(msg.chat.id, success_msg).parse_mode(MarkdownV2).await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ 오류 발생: {}", escape(&e.to_string()))).parse_mode(MarkdownV2).await?;
                }
            }
        }
        Command::List => {
            match state.db.get_keywords().await {
                Ok(keywords) => {
                    if keywords.is_empty() {
                        bot.send_message(msg.chat.id, escape("📭 등록된 키워드가 없습니다.")).parse_mode(MarkdownV2).await?;
                    } else {
                        use chrono::Utc;
                        let mut res = format!("📋 *등록된 키워드 목록 \\({}개\\)*\n\n", keywords.len());
                        let now = Utc::now();
                        
                        for k in keywords {
                            let status = if k.is_active { "🟢" } else { "🔴" };
                            let word = escape(&k.word);
                            let category = escape(&k.category.unwrap_or_else(|| "미지정".to_string()));
                            
                            // DB의 실제 값
                            let raw_interval = k.interval_min;
                            let interval_display = match raw_interval {
                                Some(val) => format!("{}분", val),
                                None => "NULL (미지정)".to_string(),
                            };
                            
                            // 스케줄러 로직용 (1440 오버라이딩 적용)
                            let interval_for_calc = raw_interval.unwrap_or(1440);
                            
                            let schedule_info = if let Some(last_run) = k.last_run {
                                let elapsed = now.signed_duration_since(last_run).num_minutes();
                                let mut remaining = interval_for_calc - elapsed;
                                if remaining < 0 { remaining = 0; }
                                
                                // UTC+9 로컬 시간으로 변환 (KST)
                                let last_run_kst = last_run + chrono::Duration::hours(9);
                                let last_run_str = escape(&last_run_kst.format("%m/%d %H:%M").to_string());
                                format!("⏳ 마지막: {} \\| ⏰ 남은 시간: *{}*분", last_run_str, remaining)
                            } else {
                                escape("✨ 실행 이력 없음 (즉시 대기중)")
                            };

                            res.push_str(&format!(
                                "{} *{}* \\(ID: `{}`\\)\n   📂 분류: {}\n   ⏱ 주기: {}\n   {}\n\n",
                                status, word, k.id, category, escape(&interval_display), schedule_info
                            ));
                        }
                        bot.send_message(msg.chat.id, res).parse_mode(MarkdownV2).await?;
                    }
                }
                Err(e) => {
                    let err_msg = format!("❌ 정보 조회 오류: {}", escape(&e.to_string()));
                    bot.send_message(msg.chat.id, err_msg).parse_mode(MarkdownV2).await?;
                }
            }
        }
        Command::Mine(word) => {
            if word.is_empty() {
                bot.send_message(msg.chat.id, escape("Usage: /mine <keyword>")).parse_mode(MarkdownV2).await?;
                return Ok(());
            }

            let keywords = match state.db.get_keywords().await {
                Ok(k) => k,
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("DB 에러 발생: {}", escape(&e.to_string()))).parse_mode(MarkdownV2).await?;
                    return Ok(());
                }
            };

            let target = keywords.into_iter().find(|k| k.word == word);
            
            if let Some(kw) = target {
                bot.send_message(msg.chat.id, format!("🔍 채굴 시작: {}", escape(&kw.word))).parse_mode(MarkdownV2).await?;
                
                let bot_clone = bot.clone();
                let chat_id = msg.chat.id;
                let state_clone = state.clone();
                
                tokio::spawn(async move {
                    match state_clone.engine.run_mining(&kw, &state_clone.db).await {
                        Ok(_) => {
                            let _ = bot_clone.send_message(chat_id, format!("✅ 채굴 완료: {}", escape(&kw.word))).parse_mode(MarkdownV2).await;
                        }
                        Err(e) => {
                            let _ = bot_clone.send_message(chat_id, format!("❌ 채굴 실패 \\({}\\): {}", escape(&kw.word), escape(&e.to_string()))).parse_mode(MarkdownV2).await;
                        }
                    }
                });
            } else {
                bot.send_message(msg.chat.id, format!("❌ 등록되지 않은 키워드입니다: {}", escape(&word))).parse_mode(MarkdownV2).await?;
            }
        }
        Command::Remove(word) => {
            if word.is_empty() {
                bot.send_message(msg.chat.id, escape("Usage: /remove <keyword>")).parse_mode(MarkdownV2).await?;
                return Ok(());
            }
            let keywords = state.db.get_keywords().await.unwrap_or_default();
            if let Some(target) = keywords.into_iter().find(|k| k.word == word) {
                match state.db.remove_keyword(target.id).await {
                    Ok(_) => {
                        bot.send_message(msg.chat.id, format!("🗑 삭제 완료: {}", escape(&word))).parse_mode(MarkdownV2).await?;
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("❌ 오류 발생: {}", escape(&e.to_string()))).parse_mode(MarkdownV2).await?;
                    }
                }
            } else {
                bot.send_message(msg.chat.id, format!("❌ 등록되지 않은 키워드입니다: {}", escape(&word))).parse_mode(MarkdownV2).await?;
            }
        }
        Command::Backup => {
            bot.send_message(msg.chat.id, escape("📦 클라우드 백업을 시작합니다...")).parse_mode(MarkdownV2).await?;
            
            let bot_clone = bot.clone();
            let state_clone = state.clone();
            
            tokio::spawn(async move {
                match state_clone.backuper.run_backup().await {
                    Ok(filename) => {
                        let msg_txt = format!("✅ 백업 및 업로드 성공\\!\n파일: {}", escape(&filename));
                        let _ = bot_clone.send_message(chat_id, msg_txt).parse_mode(MarkdownV2).await;
                    }
                    Err(e) => {
                        let _ = bot_clone.send_message(chat_id, format!("❌ 백업 실패: {}", escape(&e.to_string()))).parse_mode(MarkdownV2).await;
                    }
                }
            });
        }
        Command::Status => {
            let keywords = state.db.get_keywords().await.unwrap_or_default();
            let total_kw = keywords.len();
            let active_kw = keywords.iter().filter(|k| k.is_active).count();
            
            let today_mined = state.db.get_today_mined_count().await.unwrap_or(0);
            
            let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "./data/db/miner_main.db".to_string());
            let db_size_mb = if let Ok(meta) = std::fs::metadata(&db_path) {
                meta.len() as f64 / 1_048_576.0
            } else {
                0.0
            };

            let status_msg = format!(
                "📊 *Genesis Miner Status*\n\n\
                🔑 Keywords: {} Total \\({} Active\\)\n\
                📦 Today's Data: {} items\n\
                💾 DB Size: {} MB\n\
                ⚙️ Engine: Online\n\
                ⏱️ Scheduler: Active \\(1 min tick\\)",
                total_kw, active_kw, today_mined, escape(&format!("{:.2}", db_size_mb))
            );

            bot.send_message(msg.chat.id, status_msg).parse_mode(MarkdownV2).await?;
        }
        Command::Recent(limit_str) => {
            let limit = limit_str.trim().parse::<i64>().unwrap_or(5);
            match state.db.get_recent_mined_data(limit).await {
                Ok(data) => {
                    if data.is_empty() {
                        let _ = bot.send_message(msg.chat.id, escape("📭 수집된 데이터가 없습니다.")).parse_mode(MarkdownV2).await;
                    } else {
                        let mut res = format!("📝 *최근 수집된 데이터 \\({}건\\)*\n\n", data.len());
                        for (id, title, is_full) in data {
                            let icon = if is_full { "✅" } else { "❌" };
                            res.push_str(&format!("`{}` \\- {} {}\n", id, escape(&title), icon));
                        }
                        res.push_str(&format!("\n💡 요약은 `{}` \\| 본문은 `{}`", escape("/show [id]"), escape("/read [id]")));
                        
                        let _ = bot.send_message(msg.chat.id, res).parse_mode(MarkdownV2).await;
                    }
                }
                Err(e) => {
                    let err_msg = format!("❌ 조회 오류: {}", escape(&e.to_string()));
                    let _ = bot.send_message(msg.chat.id, err_msg).parse_mode(MarkdownV2).await;
                }
            }
        }
        Command::Show(id) => {
            match state.db.get_mined_data_by_id(id).await {
                Ok((title, content, url, is_full)) => {
                    let preview_len = 500;
                    let truncated_content = if content.len() > preview_len {
                        format!("{}...", &content[..preview_len])
                    } else {
                        content
                    };

                    let icon = if is_full { "✅" } else { "❌" };
                    let res = format!(
                        "*{}* {}\n\n{}\n\n🔗 원문 링크: {}",
                        escape(&title),
                        icon,
                        escape(&truncated_content),
                        escape(&url)
                    );
                    
                    let _ = bot.send_message(msg.chat.id, res)
                        .parse_mode(MarkdownV2)
                        .disable_web_page_preview(true)
                        .await;
                }
                Err(e) => {
                    let err_msg = format!("❌ {}", escape(&e.to_string()));
                    let _ = bot.send_message(msg.chat.id, err_msg).parse_mode(MarkdownV2).await;
                }
            }
        }
        Command::Read(id) => {
            match state.db.get_mined_data_by_id(id).await {
                Ok((title, content, url, is_full)) => {
                    let icon = if is_full { "✅ 본문 수집 완료" } else { "❌ 요약본만 존재" };
                    
                    let res = format!(
                        "*{}*\n상태: {}\n🔗 원문 링크: {}\n\n{}",
                        escape(&title),
                        escape(icon),
                        escape(&url),
                        escape(&content)
                    );

                    if res.chars().count() > 4000 {
                        use teloxide::types::InputFile;
                        let file_path = format!("./data/exports/read_{}.md", id);
                        if let Ok(_) = std::fs::write(&file_path, &content) {
                            let doc = InputFile::file(std::path::Path::new(&file_path))
                                .file_name(format!("{}.md", title.chars().take(30).collect::<String>().replace("/", "_").replace("\\", "_").replace(" ", "_")));
                            
                            let _ = bot.send_document(msg.chat.id, doc)
                                .caption(escape(&format!("📄 본문이 너무 길어 파일로 전송합니다.\n🔗 원문 링크: {}", url)))
                                .parse_mode(MarkdownV2)
                                .await;
                                
                            let _ = std::fs::remove_file(&file_path);
                        } else {
                            let _ = bot.send_message(msg.chat.id, escape("❌ 파일 생성 중 오류가 발생했습니다.")).parse_mode(MarkdownV2).await;
                        }
                    } else {
                        let _ = bot.send_message(msg.chat.id, res)
                            .parse_mode(MarkdownV2)
                            .disable_web_page_preview(true)
                            .await;
                    }
                }
                Err(e) => {
                    let err_msg = format!("❌ {}", escape(&e.to_string()));
                    let _ = bot.send_message(msg.chat.id, err_msg).parse_mode(MarkdownV2).await;
                }
            }
        }
        Command::GetFiles => {
            let bot_clone = bot.clone();
            tokio::spawn(async move {
                let _ = bot_clone.send_message(chat_id, escape("📦 마크다운 파일 압축 중...")).parse_mode(MarkdownV2).await;
                
                match create_zip_archive() {
                    Ok(zip_path) => {
                        use teloxide::types::InputFile;
                        let zip_file = InputFile::file(std::path::Path::new(&zip_path));
                        let _ = bot_clone.send_document(chat_id, zip_file).await;
                        let _ = std::fs::remove_file(&zip_path);
                    }
                    Err(e) => {
                        let _ = bot_clone.send_message(chat_id, format!("❌ 압축 실패: {}", escape(&e.to_string()))).parse_mode(MarkdownV2).await;
                    }
                }
            });
        }
    }
    Ok(())
}

fn create_zip_archive() -> anyhow::Result<String> {
    use std::fs::File;
    use std::io::{Write, Read};
    use zip::write::FileOptions;
    
    let export_dir = std::env::var("EXPORT_DIR").unwrap_or_else(|_| "./data/exports".to_string());
    let zip_path = "./data/exports_archive.zip";
    let file = File::create(zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let entries = std::fs::read_dir(&export_dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            let name = path.file_name().unwrap().to_str().unwrap();
            zip.start_file(name, options)?;
            
            let mut f = File::open(&path)?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }
    zip.finish()?;
    Ok(zip_path.to_string())
}
