use std::fs;
use std::path::Path;
fn main() {
    let base = std::env::temp_dir().join("aag_cda_probe");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("user_home")).unwrap();
    // mid-chain regular file: user_home/AppData is a file
    fs::write(base.join("user_home/AppData"), b"file").unwrap();
    let roaming = base.join("user_home/AppData/Roaming");
    println!("leaf exists(): {}", roaming.exists());
    match fs::create_dir_all(&roaming) {
        Ok(()) => println!("MID-CHAIN create_dir_all: Ok (unexpected)"),
        Err(e) => println!("MID-CHAIN create_dir_all Err: kind={:?} raw={:?}", e.kind(), e.raw_os_error()),
    }
    // leaf regular file: user_home/.cache is a file
    fs::write(base.join("user_home/.cache"), b"file").unwrap();
    let cache = base.join("user_home/.cache");
    println!("leaf exists(): {} is_dir(): {}", cache.exists(), cache.is_dir());
    match fs::create_dir_all(&cache) {
        Ok(()) => println!("LEAF create_dir_all: Ok (unexpected)"),
        Err(e) => println!("LEAF create_dir_all Err: kind={:?}", e.kind()),
    }
    // spawn with current_dir = regular file
    let mut cmd = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    cmd.current_dir(&cache).arg("/c").arg("exit");
    match cmd.spawn() {
        Ok(c) => println!("spawn with file cwd: Ok pid {:?}", c.id()),
        Err(e) => println!("spawn with file cwd Err: kind={:?} os={:?} msg={}", e.kind(), e.raw_os_error(), e),
    }
    // control: valid dir cwd
    let mut ok = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    ok.current_dir(base.join("user_home")).arg("/c").arg("exit");
    match ok.spawn() {
        Ok(c) => { println!("control spawn dir cwd: Ok pid {:?}", c.id()); let _ = c.wait(); }
        Err(e) => println!("control spawn Err: {e}"),
    }
    let _ = fs::remove_dir_all(&base);
}
