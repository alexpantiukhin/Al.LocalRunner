mod common;

use common::{err, ok};

#[test]
fn protocol_authentication_and_isolation_cannot_be_switched_off_by_policy() {
    for (text, table) in [
        ("[tls]\nenabled = false\n", "tls"),
        ("[auth]\nrequired = false\n", "auth"),
        ("[protocol]\nversion = 0\n", "protocol"),
        ("[isolation]\nenabled = false\n", "isolation"),
        ("[security]\nallow_anonymous = true\n", "security"),
    ] {
        let e = err(text);
        assert_eq!(e.table.as_deref(), Some(table), "{text}: {e}");
    }
    for (text, table, key) in [
        ("[listen]\ntls = false\n", "listen", "tls"),
        ("[listen]\nauth = \"none\"\n", "listen", "auth"),
        ("[join]\nrequire_code = false\n", "join", "require_code"),
        ("[visibility]\nshow_logs = true\n", "visibility", "show_logs"),
        ("[secrets]\nin_cache_keys = true\n", "secrets", "in_cache_keys"),
    ] {
        let e = err(text);
        assert_eq!(e.table.as_deref(), Some(table), "{text}: {e}");
        assert_eq!(e.key.as_deref(), Some(key), "{text}: {e}");
    }
    ok("");
}
