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
        .route("/api/admin/overview", get(admin::overview))
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
}
