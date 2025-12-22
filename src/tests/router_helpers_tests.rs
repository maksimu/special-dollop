use crate::router_helpers::router_url_from_ksm_config;

#[test]
fn test_router_url_prod_govcloud() {
    // PROD GOV: strip govcloud (workaround for prod infrastructure)
    let ksm_config =
        r#"{"hostname": "govcloud.keepersecurity.us", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "connect.keepersecurity.us");
}

#[test]
fn test_router_url_dev_govcloud() {
    // DEV GOV: keep govcloud in URL
    let ksm_config =
        r#"{"hostname": "govcloud.dev.keepersecurity.us", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "connect.govcloud.dev.keepersecurity.us");
}

#[test]
fn test_router_url_qa_govcloud() {
    // QA GOV: keep govcloud in URL
    let ksm_config =
        r#"{"hostname": "govcloud.qa.keepersecurity.us", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "connect.govcloud.qa.keepersecurity.us");
}

#[test]
fn test_router_url_commercial() {
    // Commercial US: no govcloud handling
    let ksm_config =
        r#"{"hostname": "keepersecurity.com", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "connect.keepersecurity.com");
}

#[test]
fn test_router_url_commercial_dev() {
    // Commercial DEV: no govcloud handling
    let ksm_config =
        r#"{"hostname": "dev.keepersecurity.com", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "connect.dev.keepersecurity.com");
}

#[test]
fn test_router_url_env_override() {
    // KPAM_ROUTER_HOST environment variable should override config
    std::env::set_var("KPAM_ROUTER_HOST", "custom.router.example.com");
    let ksm_config =
        r#"{"hostname": "keepersecurity.com", "clientId": "test", "privateKey": "test"}"#;
    let result = router_url_from_ksm_config(ksm_config).unwrap();
    assert_eq!(result, "custom.router.example.com");
    std::env::remove_var("KPAM_ROUTER_HOST");
}
