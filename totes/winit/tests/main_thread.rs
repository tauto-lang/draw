// macOS test host: run the generated Tauto program on the AppKit thread.
fn main() -> std::process::ExitCode {
    match tauto_rt::run_embedded(Default::default(), Vec::new(), winit_example::run) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}
