use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use chrono::Utc;

use crate::db::Database;

/// 검색 결과 구조체
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub content: String,
    pub url: String,
}

/// 검색 엔진 추상화 Trait
pub trait SearchProvider {
    fn name(&self) -> &'static str;
    // Rust 1.75+ Native async trait
    async fn search(&self, client: &Client, keyword: &str) -> Result<Vec<SearchResult>>;
}

// ==========================================
// Tavily Provider
// ==========================================

pub struct TavilyProvider {
    api_key: String,
}

impl TavilyProvider {
    pub fn new() -> Result<Self> {
        let api_key = env::var("TAVILY_API_KEY")
            .context("TAVILY_API_KEY 환경 변수가 설정되지 않았습니다.")?;
        Ok(Self { api_key })
    }
}

#[derive(Serialize)]
struct TavilyRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    search_depth: &'a str,
    include_answer: bool,
    max_results: u8,
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

impl SearchProvider for TavilyProvider {
    fn name(&self) -> &'static str {
        "Tavily"
    }

    async fn search(&self, client: &Client, keyword: &str) -> Result<Vec<SearchResult>> {
        let req_body = TavilyRequest {
            api_key: &self.api_key,
            query: keyword,
            search_depth: "basic",
            include_answer: false,
            max_results: 5,
        };

        let res = client.post("https://api.tavily.com/search")
            .json(&req_body)
            .send()
            .await?
            .error_for_status()?;

        let t_res: TavilyResponse = res.json().await?;
        
        Ok(t_res.results.into_iter().map(|r| SearchResult {
            title: r.title,
            content: r.content,
            url: r.url,
        }).collect())
    }
}

// ==========================================
// SearXNG Provider
// ==========================================

pub struct SearXngProvider {
    url: String,
}

impl SearXngProvider {
    pub fn new() -> Result<Self> {
        let url = env::var("SEARXNG_URL")
            .context("SEARXNG_URL 환경 변수가 설정되지 않았습니다.")?;
        Ok(Self { url })
    }
}

#[derive(Deserialize)]
struct SearXngResponse {
    results: Vec<SearXngResult>,
}

#[derive(Deserialize)]
struct SearXngResult {
    title: String,
    url: String,
    content: Option<String>,
}

impl SearchProvider for SearXngProvider {
    fn name(&self) -> &'static str {
        "SearXNG"
    }

    async fn search(&self, client: &Client, keyword: &str) -> Result<Vec<SearchResult>> {
        // q=keyword&format=json
        let res = client.get(format!("{}/search", self.url))
            .query(&[("q", keyword), ("format", "json")])
            .send()
            .await?
            .error_for_status()?;

        let s_res: SearXngResponse = res.json().await?;
        
        Ok(s_res.results.into_iter().map(|r| SearchResult {
            title: r.title,
            content: r.content.unwrap_or_default(),
            url: r.url,
        }).collect())
    }
}

// ==========================================
// Mining Engine
// ==========================================

pub enum ProviderEnum {
    Tavily(TavilyProvider),
    SearXng(SearXngProvider),
}

impl ProviderEnum {
    fn name(&self) -> &'static str {
        match self {
            Self::Tavily(p) => p.name(),
            Self::SearXng(p) => p.name(),
        }
    }

    async fn search(&self, client: &Client, keyword: &str) -> Result<Vec<SearchResult>> {
        match self {
            Self::Tavily(p) => p.search(client, keyword).await,
            Self::SearXng(p) => p.search(client, keyword).await,
        }
    }
}

pub struct MiningEngine {
    client: Client,
    providers: Vec<ProviderEnum>,
    export_dir: String,
}

impl MiningEngine {
    pub fn new() -> Result<Self> {
        let client = Client::new();
        let export_dir = env::var("EXPORT_DIR").unwrap_or_else(|_| "./data/exports".to_string());
        
        if !Path::new(&export_dir).exists() {
            fs::create_dir_all(&export_dir).context("Export 디렉토리를 생성하는 데 실패했습니다.")?;
        }

        let mut providers: Vec<ProviderEnum> = Vec::new();
        
        if let Ok(tavily) = TavilyProvider::new() {
            providers.push(ProviderEnum::Tavily(tavily));
        }
        if let Ok(searxng) = SearXngProvider::new() {
            providers.push(ProviderEnum::SearXng(searxng));
        }
        
        Ok(Self { client, providers, export_dir })
    }

    /// Hybrid Routing 검색 수행
    pub async fn run_mining(&self, keyword: &crate::db::Keyword, db: &Database) -> Result<()> {
        println!("[MiningEngine] Mining started for keyword: {}", keyword.word);
        let mut results: Option<Vec<SearchResult>> = None;

        for provider in &self.providers {
            println!("  -> Trying provider: {}", provider.name());
            match provider.search(&self.client, &keyword.word).await {
                Ok(res) => {
                    println!("    Found {} results from {}.", res.len(), provider.name());
                    results = Some(res);
                    break;
                }
                Err(e) => {
                    eprintln!("    Provider {} failed: {}", provider.name(), e);
                    // Fallback to next provider
                }
            }
        }

        let results = results.ok_or_else(|| anyhow::anyhow!("모든 검색 프로바이더가 실패했습니다: {}", keyword.word))?;

        for mut res in results {
            let mut is_full_text = false;
            
            let host = res.url.to_lowercase();
            let is_media = host.contains("youtube.com") 
                        || host.contains("youtu.be") 
                        || host.contains("vimeo.com");

            if is_media {
                println!("    -> Skipping deep scrape for media link: {}", res.url);
                res.content = format!("*원문 링크*: {}", res.url);
            } else {
                if let Some(md_content) = deep_scrape(&self.client, &res.url).await {
                    println!("    -> Deep scraping successful for: {}", res.url);
                    res.content = md_content;
                    is_full_text = true;
                } else {
                    println!("    -> Deep scraping failed, using summary fallback for: {}", res.url);
                }
            }

            // DB에 저장
            db.add_mined_data(keyword.id, &res.title, &res.content, &res.url, is_full_text).await?;
            
            // Markdown 추출
            self.export_markdown(keyword, &res, is_full_text)?;
        }
        
        // 마지막 실행 시간 업데이트
        db.update_last_run(keyword.id).await?;

        Ok(())
    }

    /// YAML Frontmatter를 포함한 마크다운 파일 생성
    fn export_markdown(&self, keyword: &crate::db::Keyword, res: &SearchResult, is_full_text: bool) -> Result<()> {
        let now = Utc::now();
        let safe_title = res.title.replace("/", "_").replace("\\", "_").replace(" ", "_");
        let category_str = keyword.category.as_deref().unwrap_or("Uncategorized");
        
        // 제한된 길이의 제목 사용, 카테고리를 파일명 앞에 포함
        let file_name = format!("[{}]{}_{}.md", category_str, now.format("%Y%m%d%H%M%S"), safe_title.chars().take(30).collect::<String>());
        let file_path = Path::new(&self.export_dir).join(&file_name);
        
        let mut file = File::create(&file_path)?;
        
        let md_content = format!(
r#"---
title: "{}"
date: {}
source: "{}"
keyword: "{}"
provider_category: "{}"
is_full_text: {}
---

# {}

{}

<br>
[출처 링크]({})
"#,
            res.title.replace('"', "\\\""),
            now.to_rfc3339(),
            res.url.replace('"', "\\\""),
            keyword.word.replace('"', "\\\""),
            keyword.category.as_deref().unwrap_or("Uncategorized").replace('"', "\\\""),
            is_full_text,
            res.title,
            res.content,
            res.url
        );

        file.write_all(md_content.as_bytes())?;
        
        Ok(())
    }
}

async fn deep_scrape(client: &Client, url: &str) -> Option<String> {
    let res = client.get(url).timeout(std::time::Duration::from_secs(10)).send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    
    // We try to get HTML text. Avoid blindly unwrap to not crash task.
    let html = res.text().await.ok()?;
    
    let document = scraper::Html::parse_document(&html);
    
    // Possible selectors for the main content
    let selectors = vec![
        "article",
        "main",
        ".content",
        "#content",
        ".post-content",
        "div[class*='content']",
    ];

    for s in selectors {
        if let Ok(selector) = scraper::Selector::parse(s) {
            if let Some(element) = document.select(&selector).next() {
                let inner_html = element.inner_html();
                let sanitized_html = ammonia::clean(&inner_html);
                let md = html2md::parse_html(&sanitized_html);
                if md.trim().len() > 100 {
                    return Some(md);
                }
            }
        }
    }
    
    // Fallback: body or nothing if body too short
    if let Ok(selector) = scraper::Selector::parse("body") {
        if let Some(element) = document.select(&selector).next() {
            let inner_html = element.inner_html();
            let sanitized_html = ammonia::clean(&inner_html);
            let md = html2md::parse_html(&sanitized_html);
            if md.trim().len() > 200 {
                return Some(md);
            }
        }
    }

    None
}
