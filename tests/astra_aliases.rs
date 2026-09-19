//! Contract A expectations are independent of discovery and route implementation.
use agent_runner_codex::models;

#[test]
fn standard_alias_mapping_and_preserved_native_families() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let label = format!("gpt-{effort}");
        assert_eq!(
            models::route(&label),
            Some(("gpt-5.6-sol", effort)),
            "{label}"
        );
    }
}

#[test]
fn compatibility_named_and_benchmark_routes_keep_native_efforts() {
    for (prefix, model) in [
        ("gpt-astra-", "gpt-6-astra"),
        ("gpt-luna-", "gpt-5.6-luna"),
        ("gpt-terra-", "gpt-5.6-terra"),
        ("gpt-sol-", "gpt-5.6-sol"),
    ] {
        assert_native_family(prefix, model);
    }
    assert_eq!(
        models::route("codex-exec-bench"),
        Some(("gpt-5.6-luna", "low"))
    );
}

fn assert_native_family(prefix: &str, model: &str) {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        assert_eq!(
            models::route(&format!("{prefix}{effort}")),
            Some((model, effort))
        );
    }
}

#[test]
fn ultra_aliases_remain_unregistered() {
    for name in [
        "gpt-ultra",
        "gpt-astra-ultra",
        "gpt-luna-ultra",
        "gpt-terra-ultra",
        "gpt-sol-ultra",
    ] {
        assert_eq!(models::route(name), None, "{name}");
    }
}

#[test]
fn removed_codex_gpt_aliases_remain_unregistered() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let name = format!("codex-gpt-{effort}");
        assert_eq!(models::route(&name), None, "{name}");
    }
}
