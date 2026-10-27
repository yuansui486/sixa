use crate::*;
#[tauri::command]
pub async fn find_text(
    state: Shared<'_>,
    id: Uuid,
    query: String,
) -> Result<task_engine::manual::TextMatches> {
    work(state, move |e| e.find_text(id, &query)).await
}
#[tauri::command]
pub async fn apply_text(
    state: Shared<'_>,
    id: Uuid,
    request: task_engine::manual::ApplyText,
) -> Result<TaskView> {
    work(state, move |e| e.apply_text(id, request)).await
}
#[tauri::command]
pub async fn undo_text(
    state: Shared<'_>,
    id: Uuid,
    mutation_id: Uuid,
    expected_revision: u64,
) -> Result<TaskView> {
    work(state, move |e| {
        e.undo_text(id, mutation_id, expected_revision)
    })
    .await
}
#[tauri::command]
pub async fn save_quick_rule(
    state: Shared<'_>,
    id: Uuid,
    name: String,
    text: String,
    replacement: String,
) -> Result<()> {
    if name.trim().is_empty()
        || text.trim().is_empty()
        || text.len() > 4096
        || replacement.len() > 4096
    {
        return Err(Error::Invalid(
            "请填写规则名称和查找文字，长度不超过 4096 字节".into(),
        ));
    }
    work(state, move |e| {
        e.store.save_quick_rule(
            &Rule {
                id,
                name: name.clone(),
                entity_type: format!("{name} · {}", &id.to_string()[..8]),
                kind: domain::RuleKind::Literal,
                pattern: text,
                enabled: true,
            },
            &replacement,
        )
    })
    .await
}
