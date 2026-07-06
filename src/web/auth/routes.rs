use axum::routing::{get, post};
use axum::Router;

use super::{admin, AuthState};

/// 管理路由（挂载前由调用方套 require_admin + require_login）。
pub fn admin_router() -> Router<AuthState> {
    Router::new()
        .route("/admin", get(admin::admin_page))
        .route("/api/admin/codes", post(admin::create_codes).get(admin::list_codes))
        .route("/api/admin/codes/revoke", post(admin::revoke_code))
        .route("/api/admin/users", get(admin::list_users))
        .route("/api/admin/users/extend", post(admin::extend_user))
        .route("/api/admin/users/disable", post(admin::disable_user))
        .route("/api/admin/users/set_admin", post(admin::set_admin))
        .route("/api/admin/users/reset_password", post(admin::reset_password))
        .route("/api/admin/users/cancel", post(admin::cancel_user))
        .route("/api/admin/users/delete", post(admin::delete_user))
        .route("/api/admin/overview", get(admin::overview))
        .route("/api/admin/push-history", get(admin::push_history_list))
        .route("/api/admin/push-history/:id", get(admin::push_history_detail))
}

#[cfg(test)]
mod tests {
    use crate::web::auth::{store, AuthState};
    use crate::web::router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AuthState {
        let conn = store::open_in_memory().unwrap();
        AuthState::new(conn, Default::default())
    }

    #[tokio::test]
    async fn core_api_requires_login() {
        let app = router(test_state());
        let resp = app
            .oneshot(Request::builder().uri("/api/funds").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logged_in_but_inactive_gets_403() {
        let state = test_state();
        let token = "tok".to_string();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "u", "h", false).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/funds")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN); // 未激活 license_required
    }

    #[tokio::test]
    async fn admin_route_hidden_for_non_admin() {
        let state = test_state();
        let token = "tok2".to_string();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "u2", "h", false).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/overview")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn push_history_hidden_for_non_admin() {
        let state = test_state();
        let token = "tok-ph".to_string();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "np", "h", false).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = crate::web::router(state);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/admin/push-history")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    /// 建立一个已登录用户会话，返回其 token；`admin`/`activated` 控制身份与授权。
    fn seed_user(state: &AuthState, name: &str, token: &str, admin: bool, activated: bool) -> i64 {
        let conn = state.db.lock().unwrap();
        let uid = store::create_user(&conn, name, "h", admin).unwrap();
        let now = chrono::Local::now().date_naive();
        if activated {
            store::set_expiry(&conn, uid, now + chrono::Duration::days(30)).unwrap();
        }
        store::create_session(&conn, token, uid, now + chrono::Duration::days(1)).unwrap();
        uid
    }

    async fn post_admin(app: axum::Router, uri: &str, token: &str, body: serde_json::Value) -> StatusCode {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", format!("xlh_session={token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn cannot_deadmin_sole_admin() {
        let state = test_state();
        let uid = seed_user(&state, "root", "atok", true, true);
        let status = post_admin(
            router(state.clone()),
            "/api/admin/users/set_admin",
            "atok",
            serde_json::json!({"user_id": uid, "is_admin": false}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "撤销唯一管理员应被拒");
        let conn = state.db.lock().unwrap();
        assert!(
            store::find_user_by_id(&conn, uid).unwrap().unwrap().is_admin,
            "唯一管理员应仍为管理员"
        );
    }

    #[tokio::test]
    async fn cannot_disable_sole_admin() {
        let state = test_state();
        let uid = seed_user(&state, "root", "atok", true, true);
        let status = post_admin(
            router(state.clone()),
            "/api/admin/users/disable",
            "atok",
            serde_json::json!({"user_id": uid, "disabled": true}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "封禁唯一管理员应被拒");
        let conn = state.db.lock().unwrap();
        assert!(
            !store::find_user_by_id(&conn, uid).unwrap().unwrap().disabled,
            "唯一管理员应仍启用"
        );
    }

    #[tokio::test]
    async fn deadmin_allowed_when_two_admins() {
        let state = test_state();
        let uid1 = seed_user(&state, "root", "atok", true, true);
        // 第二个管理员
        {
            let conn = state.db.lock().unwrap();
            store::create_user(&conn, "root2", "h", true).unwrap();
        }
        let status = post_admin(
            router(state.clone()),
            "/api/admin/users/set_admin",
            "atok",
            serde_json::json!({"user_id": uid1, "is_admin": false}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "存在第二管理员时可撤销");
        let conn = state.db.lock().unwrap();
        assert!(!store::find_user_by_id(&conn, uid1).unwrap().unwrap().is_admin);
    }

    #[tokio::test]
    async fn push_config_allowed_for_licensed_non_admin() {
        // 推送配置已按用户隔离迁到 licensed 组：已激活的非管理员应可访问自己的配置（不再要求管理员）。
        let state = test_state();
        seed_user(&state, "cust", "ctok", false, true);
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/push/config")
                    .header("cookie", "xlh_session=ctok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "已授权的非管理员应可访问自己的推送配置");
    }

    #[tokio::test]
    async fn login_rejects_cancelled_user() {
        let state = test_state();
        {
            let conn = state.db.lock().unwrap();
            let h = crate::web::auth::password::hash("pw123456").unwrap();
            let uid = store::create_user(&conn, "cx", &h, false).unwrap();
            store::set_cancelled(&conn, uid, true).unwrap();
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"cx","password":"pw123456"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn require_login_rejects_cancelled_session() {
        // 已注销用户即便持有有效会话，也应被 require_login 拦截（401）。
        let state = test_state();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "cs", "h", false).unwrap();
            store::set_cancelled(&conn, uid, true).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, "cstok", uid, exp).unwrap();
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", "xlh_session=cstok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn change_password_flow() {
        let state = test_state();
        let uid = {
            let conn = state.db.lock().unwrap();
            let h = crate::web::auth::password::hash("old123").unwrap();
            let uid = store::create_user(&conn, "pu", &h, false).unwrap();
            let now = chrono::Local::now().date_naive();
            store::create_session(&conn, "ptok", uid, now + chrono::Duration::days(1)).unwrap();
            store::create_session(&conn, "other", uid, now + chrono::Duration::days(1)).unwrap();
            uid
        };
        // 旧密码错 → 400
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"bad","new_password":"new123"})).await,
            StatusCode::BAD_REQUEST);
        // 新密码过短 → 400
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"old123","new_password":"ab"})).await,
            StatusCode::BAD_REQUEST);
        // 成功 → 200
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"old123","new_password":"new123"})).await,
            StatusCode::OK);
        let conn = state.db.lock().unwrap();
        let now = chrono::Local::now().date_naive();
        assert!(store::lookup_session_user(&conn, "other", now).unwrap().is_none(), "其他会话应失效");
        assert!(store::lookup_session_user(&conn, "ptok", now).unwrap().is_some(), "当前会话应保留");
        let h = store::pw_hash_by_id(&conn, uid).unwrap().unwrap();
        assert!(crate::web::auth::password::verify("new123", &h), "新密码可校验");
    }

    #[tokio::test]
    async fn register_blocked_at_cap() {
        let state = test_state(); // 默认 open_registration = true
        {
            let conn = state.db.lock().unwrap();
            for i in 0..10000 {
                store::create_user(&conn, &format!("u{i}"), "h", false).unwrap();
            }
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"newbie","password":"pw123456"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let j: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(j["error"], "registration_full");
    }

    #[tokio::test]
    async fn admin_reset_password_clears_sessions() {
        let state = test_state();
        seed_user(&state, "root", "atok", true, true);       // 管理员执行者
        let uid = seed_user(&state, "cust", "ctok", false, true); // 目标（已建会话 ctok）
        let s = post_admin(router(state.clone()), "/api/admin/users/reset_password", "atok",
            serde_json::json!({"user_id": uid, "new_password": "reset123"})).await;
        assert_eq!(s, StatusCode::OK);
        let conn = state.db.lock().unwrap();
        let now = chrono::Local::now().date_naive();
        assert!(store::lookup_session_user(&conn, "ctok", now).unwrap().is_none(), "目标会话应被清空");
        let h = store::pw_hash_by_id(&conn, uid).unwrap().unwrap();
        assert!(crate::web::auth::password::verify("reset123", &h));
    }

    #[tokio::test]
    async fn admin_cancel_and_delete_rules() {
        let state = test_state();
        seed_user(&state, "root", "atok", true, true); // 管理员执行者
        // 未激活账号可直接删
        let free = seed_user(&state, "free", "ftok", false, false);
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": free})).await,
            StatusCode::OK);
        // 已激活未注销 → 必须先注销
        let paid = seed_user(&state, "paid", "ptok", false, true);
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": paid})).await,
            StatusCode::BAD_REQUEST);
        // 注销 paid（会话应被清）
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/cancel", "atok",
                serde_json::json!({"user_id": paid, "cancelled": true})).await,
            StatusCode::OK);
        {
            let conn = state.db.lock().unwrap();
            let now = chrono::Local::now().date_naive();
            assert!(store::lookup_session_user(&conn, "ptok", now).unwrap().is_none());
        }
        // 已注销 → 可删
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": paid})).await,
            StatusCode::OK);
    }

    #[tokio::test]
    async fn cannot_cancel_or_delete_sole_admin() {
        let state = test_state();
        let uid = seed_user(&state, "root", "atok", true, true);
        // 注销唯一管理员被拒
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/cancel", "atok",
                serde_json::json!({"user_id": uid, "cancelled": true})).await,
            StatusCode::BAD_REQUEST);
        // 删除唯一管理员被拒（已激活且未注销，先命中 last_admin）
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": uid})).await,
            StatusCode::BAD_REQUEST);
        let conn = state.db.lock().unwrap();
        assert!(store::find_user_by_id(&conn, uid).unwrap().is_some(), "唯一管理员仍存在");
    }

    #[tokio::test]
    async fn cancel_delete_hidden_for_non_admin() {
        let state = test_state();
        seed_user(&state, "cust", "ctok", false, true); // 非管理员
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/users/delete")
                    .header("cookie", "xlh_session=ctok")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"user_id": 1}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
