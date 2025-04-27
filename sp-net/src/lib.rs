// spm-fetch/src/lib.rs
pub mod fetch;
pub mod validation;

// Re-export necessary types from sp-core IF using Option A from Step 3
// If using Option B (DTOs), you wouldn't depend on sp-core here for models.
// Re-export the public fetching functions - ensure they are `pub`
pub use fetch::api::{
    fetch_all_casks, fetch_all_formulas, fetch_cask, fetch_formula, get_cask, /* ... */
    get_formula,
};
pub use fetch::http::{fetch_formula_source_or_bottle, fetch_resource /* ... */};
pub use fetch::oci::{
    build_oci_client, /* ... */
    download_oci_blob, fetch_oci_manifest_index,
};
pub use sp_common::{
    model::{
        Cask, Formula,
        cask::{Sha256Field, UrlField},
        formula::ResourceSpec,
    }, // Example types needed
    {
        Config,
        cache::Cache,
        error::{Result, SpError},
    }, // Need Config, Result, SpError, Cache
};
pub use validation::{validate_url, verify_checksum, verify_content_type /* ... */};
