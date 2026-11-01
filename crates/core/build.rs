// `sqlx::migrate!` embeds the folder at compile time; rebuild when a migration changes.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
