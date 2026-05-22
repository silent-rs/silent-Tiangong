use tantivy::schema::*;

pub struct WorkspaceFields {
    pub path: Field,
    pub file_type: Field,
    pub size: Field,
    #[allow(dead_code)]
    pub modified_at: Field,
    pub content: Field,
    pub language: Field,
    pub symbol_name: Field,
    pub symbol_kind: Field,
    pub symbol_line_start: Field,
    pub symbol_line_end: Field,
    pub symbol_signature: Field,
}

pub fn workspace_schema() -> (Schema, WorkspaceFields) {
    let mut b = Schema::builder();
    let path = b.add_text_field("path", TEXT | STORED);
    let file_type = b.add_text_field("file_type", STRING | STORED);
    let size = b.add_u64_field("size", STORED);
    let modified_at = b.add_text_field("modified_at", STRING | STORED);
    let content = b.add_text_field("content", TEXT);
    let language = b.add_text_field("language", STRING | STORED);
    let symbol_name = b.add_text_field("symbol_name", TEXT);
    let symbol_kind = b.add_text_field("symbol_kind", STRING);
    let symbol_line_start = b.add_u64_field("symbol_line_start", STORED);
    let symbol_line_end = b.add_u64_field("symbol_line_end", STORED);
    let symbol_signature = b.add_text_field("symbol_signature", TEXT);
    (
        b.build(),
        WorkspaceFields {
            path,
            file_type,
            size,
            modified_at,
            content,
            language,
            symbol_name,
            symbol_kind,
            symbol_line_start,
            symbol_line_end,
            symbol_signature,
        },
    )
}

#[allow(dead_code)]
pub struct SessionFields {
    pub session_id: Field,
    pub workspace_id: Field,
    pub turn_id: Field,
    pub content: Field,
    pub role: Field,
    pub timestamp: Field,
    pub topics: Field,
    pub importance: Field,
    pub entity_names: Field,
}

#[allow(dead_code)]
pub fn session_schema() -> (Schema, SessionFields) {
    let mut b = Schema::builder();
    let session_id = b.add_text_field("session_id", STRING | STORED);
    let workspace_id = b.add_text_field("workspace_id", STRING);
    let turn_id = b.add_text_field("turn_id", STRING | STORED);
    let content = b.add_text_field("content", TEXT);
    let role = b.add_text_field("role", STRING | STORED);
    let timestamp = b.add_text_field("timestamp", STRING | STORED);
    let topics = b.add_text_field("topics", TEXT);
    let importance = b.add_f64_field("importance", INDEXED | STORED);
    let entity_names = b.add_text_field("entity_names", TEXT);
    (
        b.build(),
        SessionFields {
            session_id,
            workspace_id,
            turn_id,
            content,
            role,
            timestamp,
            topics,
            importance,
            entity_names,
        },
    )
}
