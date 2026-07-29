use tempfile::TempDir;
use tiangong_server::webhook::model::{UpdateWebhookRequest, Webhook};
use tiangong_server::webhook::store::WebhookStore;

fn setup() -> (TempDir, WebhookStore) {
    let dir = TempDir::new().unwrap();
    let store = WebhookStore::open_at(dir.path().to_path_buf()).unwrap();
    (dir, store)
}

fn sample_webhook(id: &str) -> Webhook {
    Webhook {
        id: id.to_string(),
        name: "测试触发".to_string(),
        description: "测试描述".to_string(),
        session_id: None,
        payload: "hello".to_string(),
        secret: None,
        enabled: true,
        created_at: chrono::Local::now().naive_local().to_string(),
        updated_at: chrono::Local::now().naive_local().to_string(),
    }
}

#[test]
fn insert_and_get_webhook() {
    let (_dir, store) = setup();
    let mut webhook = sample_webhook("wh-1");
    webhook.name = "  第一行\r\n第二行\n\n第三行  ".to_string();
    webhook.description = " 描述一\r描述二 ".to_string();
    webhook.payload = "执行第一行\r\n执行第二行".to_string();
    store.insert(&webhook).unwrap();

    // 名称/描述归一为单行，payload 保持多行原样
    let got = store.get("wh-1").unwrap().unwrap();
    assert_eq!(got.name, "第一行 第二行 第三行");
    assert_eq!(got.description, "描述一 描述二");
    assert_eq!(got.payload, "执行第一行\r\n执行第二行");
    assert!(got.session_id.is_none());

    let mut empty_webhook = sample_webhook("empty-name");
    empty_webhook.name = " \r\n ".to_string();
    let error = store.insert(&empty_webhook).unwrap_err().to_string();
    assert!(error.contains("任务名称"), "错误应指出空字段：{error}");
}

#[test]
fn update_webhook_normalizes_display_fields_without_changing_payload() {
    let (_dir, store) = setup();
    store
        .insert(&sample_webhook("wh-normalize-update"))
        .unwrap();

    store
        .update(
            "wh-normalize-update",
            &UpdateWebhookRequest {
                name: Some(" 新版\n触发 ".to_string()),
                description: Some(" 描述一\r\n\r描述二 ".to_string()),
                payload: Some("执行第一行\n执行第二行".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    let webhook = store.get("wh-normalize-update").unwrap().unwrap();
    assert_eq!(webhook.name, "新版 触发");
    assert_eq!(webhook.description, "描述一 描述二");
    assert_eq!(webhook.payload, "执行第一行\n执行第二行");

    let error = store
        .update(
            "wh-normalize-update",
            &UpdateWebhookRequest {
                description: Some(" \n\r ".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("任务描述"), "错误应指出空字段：{error}");

    // 校验失败不应改动已落盘的字段
    let webhook = store.get("wh-normalize-update").unwrap().unwrap();
    assert_eq!(webhook.description, "描述一 描述二");
}

#[test]
fn update_webhook_preserves_other_fields() {
    let (_dir, store) = setup();
    store.insert(&sample_webhook("wh-p")).unwrap();

    // 只更新 session_id
    store
        .update(
            "wh-p",
            &UpdateWebhookRequest {
                session_id: Some("sess-new".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    let got = store.get("wh-p").unwrap().unwrap();
    assert_eq!(got.session_id.as_deref(), Some("sess-new"));
    assert_eq!(got.name, "测试触发"); // 其他字段不变
    assert_eq!(got.payload, "hello");
}

#[test]
fn webhook_run_lifecycle() {
    use tiangong_server::webhook::model::{WebhookRun, WebhookRunStatus};

    let (_dir, store) = setup();
    store.insert(&sample_webhook("wh-r")).unwrap();

    let run = WebhookRun {
        id: "run-1".to_string(),
        webhook_id: "wh-r".to_string(),
        session_id: "sess-1".to_string(),
        status: WebhookRunStatus::Running,
        started_at: "2026-01-01 00:00:00".to_string(),
        finished_at: None,
        result_summary: None,
    };
    store.insert_run(&run).unwrap();

    let runs = store.list_runs("wh-r", 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert!(matches!(runs[0].status, WebhookRunStatus::Running));

    store
        .update_run_status(
            "run-1",
            "wh-r",
            &WebhookRunStatus::Succeeded,
            Some("2026-01-01 00:00:05"),
            Some("Hello"),
        )
        .unwrap();

    let runs = store.list_runs("wh-r", 10).unwrap();
    assert!(matches!(runs[0].status, WebhookRunStatus::Succeeded));
    assert_eq!(runs[0].result_summary.as_deref(), Some("Hello"));
}
