fn main() {
    // Load .env file if it exists (for local dev)
    let _ = dotenvy::dotenv();

    // Required config vars - must be set either by Doppler or .env
    let required = ["SITE_HOST", "CONVEX_HTTP_URL"];

    for var in required {
        let value = std::env::var(var)
            .unwrap_or_else(|_| panic!("{} must be set in environment or .env file", var));
        println!("cargo:rustc-env={}={}", var, value);
    }

    // Set CONFIG_DIR based on SITE_HOST
    let site_host = std::env::var("SITE_HOST").unwrap_or_default();
    let config_dir = if site_host.contains("localhost") || site_host.contains("lvh.me") {
        ".wvdsh-dev"
    } else if site_host.contains("staging") {
        ".wvdsh-stg"
    } else {
        ".wvdsh"
    };
    println!("cargo:rustc-env=CONFIG_DIR={}", config_dir);

    // Optional Cloudflare Access credentials (only needed for staging)
    let optional = ["CF_ACCESS_CLIENT_ID", "CF_ACCESS_CLIENT_SECRET"];

    for var in optional {
        if let Ok(value) = std::env::var(var) {
            println!("cargo:rustc-env={}={}", var, value);
        }
    }

    println!("cargo:rerun-if-changed=.env");
}

