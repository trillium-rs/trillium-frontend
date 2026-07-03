// Optional on stable, no-op on nightly: tell cargo to re-run the frontend build when a
// source file under ./client changes. See the `trillium_frontend::build` module docs.
fn main() {
    trillium_frontend::build::track_frontend_sources();
}
