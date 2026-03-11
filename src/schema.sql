// src/schema.sql (참고용)
/*
CREATE TABLE IF NOT EXISTS keywords (
    id INTEGER PRIMARY KEY,
    word TEXT NOT NULL UNIQUE,
    category TEXT,
    schedule TEXT, -- "daily", "hourly" 등
    last_run DATETIME
);

CREATE TABLE IF NOT EXISTS mined_data (
    id INTEGER PRIMARY KEY,
    keyword_id INTEGER,
    title TEXT,
    content TEXT,
    source_url TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(keyword_id) REFERENCES keywords(id)
);
*/