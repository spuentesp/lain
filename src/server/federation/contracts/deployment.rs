//! What a Compose file says about where its services listen.
//!
//! Only two facts are read: a service's build context (which repo its code
//! comes from) and its ports. A service that builds from this repo and
//! publishes a port is the producer of that address, from the host's side
//! (`localhost:<published>`) and from the Compose network's side
//! (`<service>:<container port>`). A service built from an image, or one that
//! publishes nothing, is not claimed: a port alone does not say which code
//! listens on it.

use serde_yaml::Value;

use crate::federation::contracts::model::ServiceName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposePort {
    /// The host port, when the port is published (`8000:8080`).
    pub host: Option<u16>,
    /// The port the container listens on.
    pub container: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeService {
    pub name: String,
    /// The build context as written, relative to the compose file's folder.
    pub build_context: Option<String>,
    pub ports: Vec<ComposePort>,
}

/// Parse the services of one compose file. Unknown keys are ignored.
pub fn parse_compose(text: &str) -> Result<Vec<ComposeService>, String> {
    let doc: Value = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    let Some(services) = doc.get("services").and_then(Value::as_mapping) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (name, body) in services {
        let Some(name) = name.as_str() else { continue };
        out.push(ComposeService {
            name: name.to_string(),
            build_context: build_context(body),
            ports: body
                .get("ports")
                .and_then(Value::as_sequence)
                .map(|items| items.iter().filter_map(parse_port).collect())
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

fn build_context(service: &Value) -> Option<String> {
    match service.get("build")? {
        Value::String(s) => Some(s.clone()),
        Value::Mapping(_) => service
            .get("build")
            .and_then(|b| b.get("context"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// A port entry in short or long form. Short: `8000:8080`, `127.0.0.1:8000:8080`,
/// `8080`, with an optional `/tcp` or `/udp`. Long: a mapping with `target` and
/// optional `published`.
fn parse_port(value: &Value) -> Option<ComposePort> {
    match value {
        Value::Number(n) => Some(ComposePort {
            host: None,
            container: n.as_u64()?.try_into().ok()?,
        }),
        Value::String(s) => parse_short(s),
        Value::Mapping(_) => {
            let container = value.get("target")?.as_u64()?.try_into().ok()?;
            let host = value
                .get("published")
                .and_then(|p| p.as_u64().or_else(|| p.as_str()?.parse().ok()))
                .and_then(|p| p.try_into().ok());
            Some(ComposePort { host, container })
        }
        _ => None,
    }
}

fn parse_short(s: &str) -> Option<ComposePort> {
    let s = s.split('/').next()?;
    let parts: Vec<&str> = s.rsplit(':').collect();
    // `container`, `host:container`, or `ip:host:container`. The ip part is
    // never a port, so only the last one or two parts matter.
    let container: u16 = parts.first()?.parse().ok()?;
    let host = parts.get(1).and_then(|p| p.parse().ok());
    Some(ComposePort { host, container })
}

/// `rel` joined to `base` (both repo-relative, `/`-separated), with `.` and
/// `..` resolved. `None` when the result leaves the repo.
pub fn join_repo_relative(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Address claims for the services a repo's compose files produce. `dir` is
/// the compose file's folder relative to the repo root (`""` for the root).
/// Each claim is `(pattern, service)`, with the pattern in `host:port` form.
pub fn producer_claims(
    dir: &str,
    services: &[ComposeService],
    owner: &ServiceName,
) -> Vec<(String, ServiceName)> {
    let mut out = Vec::new();
    for svc in services {
        let Some(context) = svc.build_context.as_deref() else {
            continue;
        };
        // A build that leaves the repo is another repo's code; not claimed here.
        if join_repo_relative(dir, context).is_none() {
            continue;
        }
        if svc.ports.is_empty() {
            continue;
        }
        for port in &svc.ports {
            out.push((format!("{}:{}", svc.name, port.container), owner.clone()));
            if let Some(host) = port.host {
                out.push((format!("localhost:{host}"), owner.clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> ServiceName {
        ServiceName("storefront-svc".into())
    }

    #[test]
    fn short_port_forms_are_read() {
        assert_eq!(
            parse_short("8000:8080"),
            Some(ComposePort {
                host: Some(8000),
                container: 8080
            })
        );
        assert_eq!(
            parse_short("127.0.0.1:8000:8080/tcp"),
            Some(ComposePort {
                host: Some(8000),
                container: 8080
            })
        );
        assert_eq!(
            parse_short("8080"),
            Some(ComposePort {
                host: None,
                container: 8080
            })
        );
    }

    #[test]
    fn long_and_numeric_port_forms_are_read() {
        let doc = "services:\n  a:\n    ports:\n      - target: 3000\n        published: 3001\n      - 80\n";
        let svcs = parse_compose(doc).unwrap();
        assert_eq!(
            svcs[0].ports,
            vec![
                ComposePort {
                    host: Some(3001),
                    container: 3000
                },
                ComposePort {
                    host: None,
                    container: 80
                },
            ]
        );
    }

    #[test]
    fn build_context_is_read_in_string_and_mapping_form() {
        let doc = "services:\n  a:\n    build: .\n  b:\n    build:\n      context: ..\n      dockerfile: x\n  c:\n    image: nginx\n";
        let svcs = parse_compose(doc).unwrap();
        assert_eq!(svcs[0].build_context.as_deref(), Some("."));
        assert_eq!(svcs[1].build_context.as_deref(), Some(".."));
        assert_eq!(svcs[2].build_context, None);
    }

    #[test]
    fn contexts_resolve_inside_the_repo_and_escapes_are_rejected() {
        assert_eq!(
            join_repo_relative(".devcontainer", ".."),
            Some(String::new())
        );
        assert_eq!(join_repo_relative("", "./app"), Some("app".into()));
        assert_eq!(join_repo_relative("", "../other"), None);
    }

    #[test]
    fn a_built_published_service_claims_both_its_addresses() {
        let svcs = parse_compose(
            "services:\n  storefront:\n    build: .\n    ports:\n      - 3000:3000\n",
        )
        .unwrap();
        let claims = producer_claims("", &svcs, &owner());
        assert!(claims.contains(&("storefront:3000".into(), owner())));
        assert!(claims.contains(&("localhost:3000".into(), owner())));
    }

    #[test]
    fn an_image_service_and_an_unpublished_service_are_not_claimed() {
        let svcs = parse_compose(
            "services:\n  dashboard:\n    image: x/dashboard\n    ports:\n      - 9000:80\n  saleor:\n    build: ..\n    command: sleep infinity\n",
        )
        .unwrap();
        assert!(producer_claims(".devcontainer", &svcs, &owner()).is_empty());
    }

    #[test]
    fn a_build_from_another_repo_is_not_claimed() {
        let svcs = parse_compose("services:\n  x:\n    build: ../other\n    ports:\n      - 1:2\n")
            .unwrap();
        assert!(producer_claims("", &svcs, &owner()).is_empty());
    }
}
