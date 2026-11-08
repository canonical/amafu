#[cfg(test)]
mod sessions {
    use std::sync::Arc;
    use std::time::Duration;

    use amafu_api::{
        ApiState, Principal, RouterTransport, TokenAuthenticator, TokenIdentity, Transport, router,
    };
    use amafu_dashboard::{DASHBOARD_HEADER, DashboardState, SessionKey, dashboard_router};
    use amafu_domain::ProjectName;
    use amafu_domain::access::Role;
    use amafu_interfaces::RandomIdentifiers;
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use http_body_util::BodyExt;
    use serde_json::json;
    use time::OffsetDateTime;
    use tower::ServiceExt;

    const MEMBER_TOKEN: &str = "amafu-member-token";
    const VIEWER_TOKEN: &str = "amafu-viewer-token";
    const PROJECT: &str = "project-a";

    fn cluster() -> Arc<dyn Transport> {
        let project = ProjectName::new(PROJECT).unwrap();
        let identity = |role, token| {
            TokenIdentity::for_token(
                Principal { name: format!("{role}-user"), role, project: project.clone() },
                token,
                None,
            )
        };
        let api = router(ApiState {
            store: Arc::new(MemoryStore::new()),
            authenticator: Arc::new(TokenAuthenticator::new(vec![
                identity(Role::Member, MEMBER_TOKEN),
                identity(Role::Viewer, VIEWER_TOKEN),
            ])),
            audit: Arc::new(MemoryAuditLog::new()),
            clock: Arc::new(VirtualClock::starting_at(OffsetDateTime::now_utc())),
            identifiers: Arc::new(RandomIdentifiers),
        });
        Arc::new(RouterTransport::new(api))
    }

    fn dashboard_instances(key: &str, count: usize, cluster: &Arc<dyn Transport>) -> Vec<axum::Router> {
        (0..count)
            .map(|_| {
                let state = DashboardState::new(
                    SessionKey::from_hexadecimal(key).unwrap(),
                    vec![Arc::clone(cluster)],
                    Duration::from_secs(3600),
                );
                dashboard_router(state)
            })
            .collect()
    }

    async fn send(
        instance: &axum::Router,
        request: Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = instance.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    async fn sign_in(instance: &axum::Router, token: &str) -> (StatusCode, Option<String>) {
        let body = json!({"token": token, "project": PROJECT}).to_string();
        let request = Request::post("/session")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let (status, headers, _) = send(instance, request).await;
        let cookie = headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned);
        (status, cookie)
    }

    fn with_cookie(
        method: Method,
        path: &str,
        cookie: &str,
        body: Option<serde_json::Value>,
    ) -> Request<Body> {
        let mut request = Request::builder().method(method).uri(path).header(header::COOKIE, cookie);
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json").header(DASHBOARD_HEADER, "1");
        }
        request.body(body.map_or_else(Body::empty, |body| Body::from(body.to_string()))).unwrap()
    }

    fn small_vm() -> serde_json::Value {
        json!({"metadata": {"name": "web-1"}, "specification": {"cpu_count": 1, "memory_mebibytes": 128, "boot": {"type": "kernel", "bundle": "test-guest"}}})
    }

    #[tokio::test]
    async fn a_session_survives_the_loss_of_the_instance_that_issued_it() {
        let cluster = cluster();
        let key = SessionKey::generate_hexadecimal().unwrap();
        let mut instances = dashboard_instances(&key, 3, &cluster);
        let (status, cookie) = sign_in(&instances[0], MEMBER_TOKEN).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = cookie.unwrap();
        let created = send(
            &instances[0],
            with_cookie(Method::POST, &format!("/api/v1/vms?project={PROJECT}"), &cookie, Some(small_vm())),
        )
        .await;
        assert_eq!(created.0, StatusCode::CREATED, "{:?}", created.2);
        drop(instances.remove(0));
        for survivor in &instances {
            let (status, _, listed) = send(
                survivor,
                with_cookie(Method::GET, &format!("/api/v1/vms?project={PROJECT}"), &cookie, None),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(listed["items"][0]["metadata"]["name"], "web-1");
        }
        let stranger = &dashboard_instances(&SessionKey::generate_hexadecimal().unwrap(), 1, &cluster)[0];
        let (status, _, _) = send(
            stranger,
            with_cookie(Method::GET, &format!("/api/v1/vms?project={PROJECT}"), &cookie, None),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_dashboard_can_do_only_what_its_user_may() {
        let cluster = cluster();
        let instance = &dashboard_instances(&SessionKey::generate_hexadecimal().unwrap(), 1, &cluster)[0];
        assert_eq!(sign_in(instance, "amafu-unknown-token").await.0, StatusCode::UNAUTHORIZED);
        let (_, cookie) = sign_in(instance, VIEWER_TOKEN).await;
        let cookie = cookie.unwrap();
        let (status, _, _) = send(
            instance,
            with_cookie(Method::POST, &format!("/api/v1/vms?project={PROJECT}"), &cookie, Some(small_vm())),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let unsigned = Request::get(format!("/api/v1/vms?project={PROJECT}")).body(Body::empty()).unwrap();
        assert_eq!(send(instance, unsigned).await.0, StatusCode::UNAUTHORIZED);
        let (_, member) = sign_in(instance, MEMBER_TOKEN).await;
        let forged = Request::post(format!("/api/v1/vms?project={PROJECT}"))
            .header(header::COOKIE, member.unwrap())
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(small_vm().to_string()))
            .unwrap();
        assert_eq!(send(instance, forged).await.0, StatusCode::FORBIDDEN);
    }

    async fn next_event(body: &mut Body) -> serde_json::Value {
        loop {
            let frame =
                tokio::time::timeout(Duration::from_secs(5), body.frame()).await.unwrap().unwrap().unwrap();
            let Ok(data) = frame.into_data() else { continue };
            let text = String::from_utf8(data.to_vec()).unwrap();
            if let Some(line) = text.lines().find_map(|line| line.strip_prefix("data: ")) {
                return serde_json::from_str(line).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn changes_reach_the_browser_as_they_happen() {
        let cluster = cluster();
        let instance = &dashboard_instances(&SessionKey::generate_hexadecimal().unwrap(), 1, &cluster)[0];
        let (_, cookie) = sign_in(instance, MEMBER_TOKEN).await;
        let cookie = cookie.unwrap();
        let unsigned = Request::get(format!("/events/vms?project={PROJECT}")).body(Body::empty()).unwrap();
        assert_eq!(send(instance, unsigned).await.0, StatusCode::UNAUTHORIZED);
        let response = instance
            .clone()
            .oneshot(with_cookie(Method::GET, &format!("/events/vms?project={PROJECT}"), &cookie, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        assert_eq!(next_event(&mut body).await, json!({"type": "snapshot", "items": []}));
        let created = send(
            instance,
            with_cookie(Method::POST, &format!("/api/v1/vms?project={PROJECT}"), &cookie, Some(small_vm())),
        )
        .await;
        assert_eq!(created.0, StatusCode::CREATED);
        let changed = next_event(&mut body).await;
        assert_eq!(changed["type"], "changed");
        assert_eq!(changed["object"]["metadata"]["name"], "web-1");
        let deleted = send(
            instance,
            with_cookie(
                Method::DELETE,
                &format!("/api/v1/vms/web-1?project={PROJECT}"),
                &cookie,
                Some(json!({})),
            ),
        )
        .await;
        assert_eq!(deleted.0, StatusCode::ACCEPTED, "{:?}", deleted.2);
        let deleting = next_event(&mut body).await;
        assert_eq!(deleting["object"]["metadata"]["is_deleting"], true);
    }

    #[tokio::test]
    async fn the_interface_is_served() {
        let cluster = cluster();
        let instance = &dashboard_instances(&SessionKey::generate_hexadecimal().unwrap(), 1, &cluster)[0];
        for path in ["/", "/app.js", "/style.css"] {
            let response =
                instance.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }
}
