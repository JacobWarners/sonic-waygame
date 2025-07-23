/*
 * src/main.rs
 *
 * This is the main source code for the Rust application.
 */

use evdev::{Device, InputEventKind, Key};
use nix::fcntl::{flock, FlockArg};
use rand::Rng;
use rodio::{source::Source, Decoder, OutputStream, Sink}; // Corrected: Added 'Source'
use std::env; // New: For reading command-line arguments
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// --- Configuration ---
const COUNTER_FILE: &str = "/tmp/waybar_counter.txt";
const WORKSPACE_STATE_FILE: &str = "/tmp/waybar_status.txt";
const RESET_COUNTER_ON_START: bool = true;

// --- IMPORTANT: Update these with the actual paths to your sound files ---
const SPECIAL_MODE_SOUND_1: &str = "/home/jake/Music/Super-Sonic-Transform.mp3";
const SPECIAL_MODE_SOUND_2: &str = "/home/jake/Music/Super-sonic-song.mp3";
const INCREMENT_SOUND: &str = "/home/jake/Music/Sonic-Ring.mp3";

// Hints to find the correct keyboards
const KEYBOARD_HINTS: &[&str] = &["GMMK Pro Keyboard", "Translated"];

// --- Game Difficulty Modes ---
#[derive(Clone, Copy, Debug)]
enum GameMode {
    Test,
    Normal,
    Hard,
}

// --- Shared Application State ---
// This struct holds all the data that needs to be shared between threads.
struct AppState {
    counter: u32,
    backslash_count: u8,
    is_decrementing: bool,
    keystroke_buffer: u32,
    target_keystrokes: u32,
    game_mode: GameMode,
}

// --- Commands for the Audio Thread ---
enum AudioCommand {
    Play(Vec<String>),
    PlayAndLoop { intro: String, looping: String },
    Stop,
}

fn main() -> Result<(), Box<dyn Error>> {
    // --- Parse Command-Line Arguments ---
    let mut game_mode = GameMode::Normal; // Default mode
    if let Some(arg) = env::args().nth(1) {
        match arg.as_str() {
            "--test" => game_mode = GameMode::Test,
            "--normal" => game_mode = GameMode::Normal,
            "--hard" => game_mode = GameMode::Hard,
            _ => println!("WARNING: Unknown argument '{}'. Defaulting to normal mode.", arg),
        }
    }
    println!("INFO: Starting in {:?} mode.", game_mode);

    // Set the initial random target based on the selected game mode.
    let initial_target = match game_mode {
        GameMode::Test => 1,
        GameMode::Normal => rand::thread_rng().gen_range(1..=100),
        GameMode::Hard => rand::thread_rng().gen_range(1..=1000),
    };

    // Initialize the shared state
    let state = Arc::new(Mutex::new(AppState {
        counter: 0,
        backslash_count: 0,
        is_decrementing: false,
        keystroke_buffer: 0,
        target_keystrokes: initial_target,
        game_mode,
    }));

    // Create a channel for sending commands to the audio thread
    let (audio_tx, audio_rx) = mpsc::channel();

    // Spawn the dedicated audio thread
    thread::spawn(move || {
        audio_thread_loop(audio_rx);
    });

    // Initialize or reset the counter and workspace state files
    if RESET_COUNTER_ON_START || !Path::new(COUNTER_FILE).exists() {
        write_to_file(COUNTER_FILE, "0")?;
        write_to_file(WORKSPACE_STATE_FILE, "flashing")?;
    } else {
        // On start, load the counter from the file into our state
        let mut state_guard = state.lock().unwrap();
        state_guard.counter = read_from_file(COUNTER_FILE)?.parse().unwrap_or(0);
    }

    // --- Find and spawn listeners for all specified keyboards ---
    let devices = evdev::enumerate().collect::<Vec<_>>();
    for hint in KEYBOARD_HINTS {
        if let Some(path) = find_device_path(&devices, hint) {
            println!("INFO: Found keyboard matching '{}' at {}", hint, path.display());
            let state_clone = Arc::clone(&state);
            let audio_tx_clone = audio_tx.clone();
            thread::spawn(move || {
                if let Err(e) = event_listener(path, state_clone, audio_tx_clone) {
                    eprintln!("ERROR: Listener thread for {} failed: {}", hint, e);
                }
            });
        } else {
            eprintln!("WARNING: Could not find a keyboard device matching '{}'", hint);
        }
    }

    // Keep the main thread alive indefinitely
    loop {
        thread::park();
    }
}

// --- Event Listener Thread ---
fn event_listener(
    path: PathBuf,
    state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AudioCommand>,
) -> Result<(), Box<dyn Error>> {
    let mut device = Device::open(&path)?;
    println!("INFO: Started listener on {}", path.display());
    loop {
        for ev in device.fetch_events()? {
            // We only care about key presses (value 1)
            if ev.value() == 1 {
                if let InputEventKind::Key(key) = ev.kind() {
                    process_key_event(key.code(), Arc::clone(&state), audio_tx.clone())?;
                }
            }
        }
    }
}

// --- Main Game Logic ---
fn process_key_event(
    key_code: u16,
    state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AudioCommand>,
) -> Result<(), Box<dyn Error>> {
    let mut state_guard = state.lock().unwrap();

    // Key code for 'ESC' is 1
    if key_code == Key::KEY_ESC.code() {
        println!("ACTION: Escape key pressed. Stopping audio.");
        audio_tx.send(AudioCommand::Stop)?;
        return Ok(());
    }

    // If the decrementer is running, ignore all other key presses.
    if state_guard.is_decrementing {
        return Ok(());
    }

    // Key code for '\' is 43 (KEY_BACKSLASH)
    if key_code == Key::KEY_BACKSLASH.code() && state_guard.counter >= 50 {
        state_guard.backslash_count += 1;
        println!("INFO: Backslash pressed. Count: {}", state_guard.backslash_count);

        if state_guard.backslash_count >= 3 {
            println!("ACTION: Special mode triggered!");
            state_guard.is_decrementing = true;
            state_guard.backslash_count = 0;
            write_to_file(WORKSPACE_STATE_FILE, "super-charge-flash")?;

            // Send a command to play the intro and then loop the main song.
            audio_tx.send(AudioCommand::PlayAndLoop {
                intro: SPECIAL_MODE_SOUND_1.to_string(),
                looping: SPECIAL_MODE_SOUND_2.to_string(),
            })?;

            // Spawn a new thread for the decrementer, passing it the audio sender
            let state_clone = Arc::clone(&state);
            let audio_tx_clone = audio_tx.clone();
            thread::spawn(move || {
                decrementer_loop(state_clone, audio_tx_clone);
            });
        }
    } else {
        // On any other key, reset the backslash count and handle keystroke buffering.
        state_guard.backslash_count = 0;
        state_guard.keystroke_buffer += 1;

        // Check if the buffer has reached the random target.
        if state_guard.keystroke_buffer >= state_guard.target_keystrokes {
            // Reset the buffer
            state_guard.keystroke_buffer = 0;
            // Increment the main counter
            state_guard.counter += 1;
            // Set a new random target for the next increment based on the game mode.
            state_guard.target_keystrokes = match state_guard.game_mode {
                GameMode::Test => 1,
                GameMode::Normal => rand::thread_rng().gen_range(1..=100),
                GameMode::Hard => rand::thread_rng().gen_range(1..=1000),
            };

            println!(
                "ACTION: Counter incremented to {}. Next increment in {} keystrokes.",
                state_guard.counter, state_guard.target_keystrokes
            );

            // Play the increment sound
            audio_tx.send(AudioCommand::Play(vec![INCREMENT_SOUND.to_string()]))?;

            // Update the counter file for Waybar to read.
            write_to_file(COUNTER_FILE, &state_guard.counter.to_string())?;
        }
    }

    Ok(())
}

// --- Decrementer Thread ---
fn decrementer_loop(state: Arc<Mutex<AppState>>, audio_tx: Sender<AudioCommand>) {
    loop {
        thread::sleep(Duration::from_secs(1));
        let mut state_guard = state.lock().unwrap();

        if state_guard.counter > 0 {
            state_guard.counter -= 1;
            if let Err(e) = write_to_file(COUNTER_FILE, &state_guard.counter.to_string()) {
                eprintln!("ERROR: Failed to write to counter file: {}", e);
            }
        } else {
            println!("INFO: Decrementer finished. Resetting state and stopping music.");
            state_guard.is_decrementing = false;
            if let Err(e) = write_to_file(WORKSPACE_STATE_FILE, "flashing") {
                eprintln!("ERROR: Failed to write to workspace state file: {}", e);
            }
            // Send the stop command to the audio thread
            if let Err(e) = audio_tx.send(AudioCommand::Stop) {
                eprintln!("ERROR: Failed to send Stop command from decrementer: {}", e);
            }
            break;
        }
    }
}

// --- Dedicated Audio Thread ---
fn audio_thread_loop(rx: mpsc::Receiver<AudioCommand>) {
    // Get an output stream handle to the default physical sound device
    let (_stream, stream_handle) = match OutputStream::try_default() {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("ERROR: Could not get audio output stream: {}", e);
            return;
        }
    };

    // Create a sink to play sounds
    let sink = match Sink::try_new(&stream_handle) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: Could not create audio sink: {}", e);
            return;
        }
    };

    // This loop waits for commands from the main application.
    for command in rx {
        match command {
            AudioCommand::Play(sound_paths) => {
                println!("AUDIO: Received Play command.");
                sink.stop();

                for path_str in sound_paths.iter() {
                    if let Ok(file) = File::open(path_str) {
                        if let Ok(source) = Decoder::new(BufReader::new(file)) {
                            sink.append(source);
                        }
                    }
                }
                sink.play();
            }
            AudioCommand::PlayAndLoop { intro, looping } => {
                println!("AUDIO: Received PlayAndLoop command.");
                sink.stop();

                // Append the intro sound (plays once)
                if let Ok(file) = File::open(&intro) {
                    if let Ok(source) = Decoder::new(BufReader::new(file)) {
                        sink.append(source);
                    }
                }

                // For the looping sound, decode it into an in-memory buffer
                // to allow for seamless, gapless looping.
                if let Ok(file) = File::open(&looping) {
                    if let Ok(source) = Decoder::new(BufReader::new(file)) {
                        // Collect all the decoded audio samples into a vector
                        let samples: Vec<i16> = source.convert_samples().collect();
                        // Create a new source that infinitely cycles through the in-memory samples
                        let looping_source =
                            rodio::source::from_iter(samples.into_iter().cycle());
                        sink.append(looping_source);
                    }
                }
                sink.play();
            }
            AudioCommand::Stop => {
                println!("AUDIO: Received Stop command.");
                sink.stop();
            }
        }
    }
}

// --- Utility Functions ---

// Finds the path of the first device that contains the hint in its name.
fn find_device_path(devices: &[(PathBuf, Device)], hint: &str) -> Option<PathBuf> {
    devices
        .iter()
        .find(|(_path, device)| device.name().map_or(false, |name| name.contains(hint)))
        .map(|(path, _device)| path.clone())
}

// Helper to read from a file, with a file lock for safety.
fn read_from_file(path: &str) -> Result<String, Box<dyn Error>> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    flock(file.as_raw_fd(), FlockArg::LockShared)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

// Helper to write to a file, with a file lock for safety.
fn write_to_file(path: &str, content: &str) -> Result<(), Box<dyn Error>> {
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    flock(file.as_raw_fd(), FlockArg::LockExclusive)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

