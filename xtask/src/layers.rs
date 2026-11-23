use crate::model::{Crate, Dep, DepKind};

const LAYERS: &[(&str, u8)] = &[
    ("domain", 0),
    ("db", 1),
    ("i18n", 1),
    ("pos-core", 1),
    ("auth", 2),
    ("catalog", 2),
    ("bakedays", 3),
    ("recipes", 3),
    ("orders", 4),
    ("labels", 4),
    ("payments", 5),
    ("production", 5),
    ("notify", 5),
    ("ai", 5),
    ("reports", 6),
    ("jobs", 6),
    ("server", 6),
    ("pos", 6),
    ("cli", 6),
];
const TOP_LAYER: u8 = 6;
const TESTKIT: &str = "testkit";
const EXCLUDED: &[&str] = &["xtask"];
const HTTP_CRATES: &[&str] = &[
    "actix-web",
    "axum",
    "axum-core",
    "axum-extra",
    "headers",
    "http",
    "http-body",
    "http-body-util",
    "hyper",
    "hyper-util",
    "poem",
    "tower-http",
    "tower-sessions",
    "warp",
];

fn layer(name: &str) -> Option<u8> {
    LAYERS
        .iter()
        .find(|(crate_name, _)| *crate_name == name)
        .map(|(_, layer)| *layer)
}

fn label(name: &str) -> String {
    match layer(name) {
        Some(layer) => format!("{name} (layer {layer})"),
        None if name == TESTKIT => format!("{name} (test-only)"),
        None => format!("{name} (no layer)"),
    }
}

fn kind_name(kind: DepKind) -> &'static str {
    match kind {
        DepKind::Normal => "dependency",
        DepKind::Build => "build-dependency",
        DepKind::Dev => "dev-dependency",
    }
}

pub fn check(crates: &[Crate]) -> Vec<String> {
    let mut violations = Vec::new();
    for krate in crates
        .iter()
        .filter(|krate| !EXCLUDED.contains(&krate.name.as_str()))
    {
        let name = krate.name.as_str();
        if name != TESTKIT && layer(name).is_none() {
            violations.push(format!(
                "{name} is not in the layer table; add it to LAYERS in xtask/src/layers.rs"
            ));
            continue;
        }
        if name == "server"
            && krate
                .default_features
                .iter()
                .any(|feature| feature == "ai" || feature == "dep:ai")
        {
            violations.push(format!(
                "{}: the default features enable ai; ai must stay opt-in",
                label(name)
            ));
        }
        for dep in &krate.deps {
            if dep.workspace {
                check_workspace_dep(name, dep, &mut violations);
            } else if name != "server" && HTTP_CRATES.contains(&dep.name.as_str()) {
                violations.push(format!(
                    "{} has {} {}; only server may depend on HTTP crates",
                    label(name),
                    kind_name(dep.kind),
                    dep.name
                ));
            }
        }
    }
    violations
}

fn check_workspace_dep(name: &str, dep: &Dep, violations: &mut Vec<String>) {
    let dep_name = dep.name.as_str();
    let edge = format!(
        "{} has {} {}",
        label(name),
        kind_name(dep.kind),
        label(dep_name)
    );
    if EXCLUDED.contains(&dep_name) {
        violations.push(format!("{edge}; no crate may depend on {dep_name}"));
        return;
    }
    if dep_name == TESTKIT {
        if dep.kind != DepKind::Dev {
            violations.push(format!("{edge}; testkit may only be a dev-dependency"));
        }
        return;
    }
    let Some(dep_layer) = layer(dep_name) else {
        return;
    };
    if name == TESTKIT {
        if dep_layer >= TOP_LAYER {
            violations.push(format!(
                "{edge}; testkit may only depend on crates below layer {TOP_LAYER}"
            ));
        }
        return;
    }
    if layer(name).is_some_and(|own| dep_layer >= own) {
        violations.push(format!(
            "{edge}; a crate may only depend on crates in a lower layer"
        ));
    }
    if name == "server" && dep_name == "ai" && dep.kind != DepKind::Dev && !dep.optional {
        violations.push(format!(
            "{edge} that is not optional; ai must be optional and enabled by a feature"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dep, DepKind};

    fn ws(name: &str, kind: DepKind) -> Dep {
        Dep {
            name: name.to_string(),
            kind,
            optional: false,
            workspace: true,
        }
    }

    fn ext(name: &str) -> Dep {
        Dep {
            name: name.to_string(),
            kind: DepKind::Normal,
            optional: false,
            workspace: false,
        }
    }

    fn krate(name: &str, deps: Vec<Dep>) -> Crate {
        Crate {
            name: name.to_string(),
            deps,
            default_features: Vec::new(),
        }
    }

    fn clean_workspace() -> Vec<Crate> {
        vec![
            krate("domain", vec![ext("jiff"), ws("testkit", DepKind::Dev)]),
            krate("pos-core", vec![ws("domain", DepKind::Normal)]),
            krate(
                "orders",
                vec![ws("domain", DepKind::Normal), ws("catalog", DepKind::Build)],
            ),
            krate("catalog", vec![ws("domain", DepKind::Normal)]),
            krate("testkit", vec![ws("orders", DepKind::Normal)]),
            krate(
                "server",
                vec![
                    ext("axum"),
                    ws("orders", DepKind::Normal),
                    Dep {
                        optional: true,
                        ..ws("ai", DepKind::Normal)
                    },
                ],
            ),
            krate("ai", vec![]),
            krate("xtask", vec![ext("http"), ws("server", DepKind::Normal)]),
        ]
    }

    #[test]
    fn clean_workspace_passes() {
        assert_eq!(check(&clean_workspace()), Vec::<String>::new());
    }

    #[test]
    fn rule_a_lower_layer_depending_on_higher_layer_fails() {
        let mut crates = clean_workspace();
        crates[0].deps.push(ws("orders", DepKind::Normal));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("domain (layer 0)"), "{violations:?}");
        assert!(violations[0].contains("orders (layer 4)"), "{violations:?}");
    }

    #[test]
    fn rule_a_same_layer_dependency_fails() {
        let mut crates = clean_workspace();
        crates.push(krate("labels", vec![ws("orders", DepKind::Normal)]));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("labels (layer 4)"), "{violations:?}");
    }

    #[test]
    fn rule_a_dev_dependency_upwards_fails() {
        let mut crates = clean_workspace();
        crates[3].deps.push(ws("orders", DepKind::Dev));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("catalog (layer 2)"),
            "{violations:?}"
        );
    }

    #[test]
    fn rule_a_testkit_as_normal_dependency_fails() {
        let mut crates = clean_workspace();
        crates[2].deps.push(ws("testkit", DepKind::Normal));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("testkit"), "{violations:?}");
        assert!(violations[0].contains("orders"), "{violations:?}");
    }

    #[test]
    fn rule_a_testkit_depending_on_layer_6_fails() {
        let mut crates = clean_workspace();
        crates[4].deps.push(ws("server", DepKind::Normal));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("server (layer 6)"), "{violations:?}");
    }

    #[test]
    fn rule_c_http_crate_outside_server_fails() {
        let mut crates = clean_workspace();
        crates[2].deps.push(ext("tower-http"));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("orders (layer 4)"), "{violations:?}");
        assert!(violations[0].contains("tower-http"), "{violations:?}");
    }

    #[test]
    fn rule_d_server_with_non_optional_ai_fails() {
        let mut crates = clean_workspace();
        crates[5].deps[2].optional = false;
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("server (layer 6)"), "{violations:?}");
        assert!(violations[0].contains("ai (layer 5)"), "{violations:?}");
        assert!(violations[0].contains("optional"), "{violations:?}");
    }

    #[test]
    fn nothing_may_depend_on_xtask() {
        let mut crates = clean_workspace();
        crates[0].deps.push(ws("xtask", DepKind::Normal));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("domain (layer 0)"), "{violations:?}");
        assert!(violations[0].contains("xtask"), "{violations:?}");
    }

    #[test]
    fn rule_c_axum_core_outside_server_fails() {
        let mut crates = clean_workspace();
        crates[0].deps.push(ext("axum-core"));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("axum-core"), "{violations:?}");
    }

    #[test]
    fn rule_d_server_dev_dependency_on_ai_passes() {
        let mut crates = clean_workspace();
        crates[5].deps.push(ws("ai", DepKind::Dev));
        assert_eq!(check(&crates), Vec::<String>::new());
    }

    #[test]
    fn rule_d_ai_in_server_default_features_fails() {
        let mut crates = clean_workspace();
        crates[5].default_features = vec!["ai".to_string()];
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("default"), "{violations:?}");
    }

    #[test]
    fn rule_e_crate_missing_from_layer_table_fails() {
        let mut crates = clean_workspace();
        crates.push(krate("invoices", vec![ws("domain", DepKind::Normal)]));
        let violations = check(&crates);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("invoices"), "{violations:?}");
        assert!(violations[0].contains("layer table"), "{violations:?}");
    }
}
