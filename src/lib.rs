mod backend;varnish::boilerplate!();

use std::error::Error;

use varnish::vcl::ctx::Ctx;
use varnish::vcl::backend::{Backend, VCLBackendPtr};
use crate::backend::{FileBackend, FileTransfer};

#[allow(non_camel_case_types)]
struct root {
    backend: Backend<FileBackend, FileTransfer>,
}

impl root {
    pub fn new(
        ctx: &mut Ctx,
        vcl_name: &str,
        path: &str,
    ) -> Result<Self, Box<dyn Error>> {
        if path.is_empty() {
            return Err(format!("fileserver: can't create {} with an empty path", vcl_name).into());
        }

        let backend = FileBackend {
            path: path.to_string(),
        };

        let backend = Backend::new(ctx, vcl_name, backend, false)?;

        Ok(root { backend })
    }

    pub fn backend(&self, _ctx: &Ctx) -> VCLBackendPtr {
        self.backend.vcl_ptr()
    }
}
