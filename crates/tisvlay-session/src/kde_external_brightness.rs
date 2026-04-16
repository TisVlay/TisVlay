//! Generated client bindings for kde-external-brightness-v1.

#![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
#![allow(non_upper_case_globals, non_snake_case, unused_imports)]
#![allow(missing_docs, clippy::all)]

pub(crate) mod client {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_client::backend as wayland_backend;

    pub(crate) mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/kde-external-brightness-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/kde-external-brightness-v1.xml");
}
