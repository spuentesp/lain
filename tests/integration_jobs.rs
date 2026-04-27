//! Integration tests for Lain job management and webhooks

#[tokio::test]
async fn jobs_persist_and_webhook() {
    // Setup temp job store path
    let tmp = tempfile::tempdir().unwrap();
    let job_store = tmp.path().join("jobs.json");
    std::env::set_var("LAIN_JOB_STORE", job_store.to_string_lossy().to_string());

    // Create executor with minimal dependencies
    let temp_dir = tempfile::tempdir().unwrap();
    let graph = lain::graph::GraphDatabase::new(&temp_dir.path().join("graph.bin")).unwrap();
    let exec = lain::tools::create_test_executor_with_graph(graph);

    // Start background debug_sleep job
    let mut args = serde_json::Map::new();
    args.insert("secs".into(), serde_json::Value::Number(serde_json::Number::from(1)));
    args.insert("background".into(), serde_json::Value::Bool(true));
    let resp = exec.call("debug_sleep", Some(&args)).await.unwrap();

    // Response should contain job_id
    let val: serde_json::Value = serde_json::from_str(&resp).expect("parse job response");
    let job_id = val.get("job_id").and_then(|v| v.as_str()).expect("job_id present");
    assert!(!job_id.is_empty());

    // Wait for job to complete - give it plenty of time
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    // Check job status
    let mut status_args = serde_json::Map::new();
    status_args.insert("job_id".into(), serde_json::Value::String(job_id.to_string()));
    let status_resp = exec.call("get_job_status", Some(&status_args)).await.unwrap();

    let status_val: serde_json::Value = serde_json::from_str(&status_resp).expect("parse status response");

    // Job should exist and be completed
    assert_eq!(status_val.get("id").and_then(|v| v.as_str()), Some(job_id));

    // Check the state - the structure is {"Running": null} or {"Completed": {"success": true, ...}}
    if let Some(state_obj) = status_val.get("state").and_then(|v| v.as_object()) {
        if let Some(completed) = state_obj.get("Completed").and_then(|v| v.as_object()) {
            assert_eq!(completed.get("success"), Some(&serde_json::Value::Bool(true)),
                "Job should have completed successfully");
        } else {
            panic!("Job should be in Completed state, got: {}", status_resp);
        }
    } else {
        panic!("Job state missing, got: {}", status_resp);
    }

    // Jobs file should exist and contain job id
    assert!(job_store.exists(), "Job store file should exist");
    let contents = std::fs::read_to_string(&job_store).unwrap();
    assert!(contents.contains(job_id), "jobs file should include the created job id");
}

#[tokio::test]
async fn job_status_not_found() {
    let temp_dir = tempfile::tempdir().unwrap();
    let graph = lain::graph::GraphDatabase::new(&temp_dir.path().join("graph.bin")).unwrap();
    let exec = lain::tools::create_test_executor_with_graph(graph);

    // Try to get status of non-existent job
    let mut args = serde_json::Map::new();
    args.insert("job_id".into(), serde_json::Value::String("nonexistent".to_string()));
    let result = exec.call("get_job_status", Some(&args)).await;

    // Should return an error
    assert!(result.is_err());
}

#[tokio::test]
async fn foreground_job_executes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let graph = lain::graph::GraphDatabase::new(&temp_dir.path().join("graph.bin")).unwrap();
    let exec = lain::tools::create_test_executor_with_graph(graph);

    // Run foreground debug_sleep job
    let mut args = serde_json::Map::new();
    args.insert("secs".into(), serde_json::Value::Number(serde_json::Number::from(1)));
    // No background flag = foreground

    let resp = exec.call("debug_sleep", Some(&args)).await.unwrap();

    // Foreground job returns directly, not a job_id
    assert!(resp.contains("Slept for 1 second(s)"), "Foreground job should complete immediately");
}
