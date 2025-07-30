use charms_app_runner::{AppProverInput, AppProverOutput};
use charms_data::util;

pub fn main() {
    // Read an input to the program.
    let input_vec = sp1_zkvm::io::read_vec();
    let input: AppProverInput = util::read(input_vec.as_slice()).unwrap();

    let output = run(input);

    eprintln!("about to commit");

    // Commit to the public values of the program.
    let output_vec = util::write(&output).unwrap();
    sp1_zkvm::io::commit_slice(output_vec.as_slice());
}

fn run(input: AppProverInput) -> AppProverOutput {
    let app_runner = charms_app_runner::AppRunner::new();
    let AppProverInput {
        app_binaries,
        tx,
        app_public_inputs,
        app_private_inputs,
    } = input;
    let cycles = app_runner
        .run_all(&app_binaries, &tx, &app_public_inputs, &app_private_inputs)
        .expect("all apps should run successfully");
    AppProverOutput {
        tx,
        app_public_inputs,
        cycles,
    }
}
