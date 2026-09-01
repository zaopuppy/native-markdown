use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn send_request(
    stdin: &mut impl Write,
    stdout: &mut impl Read,
    id: u64,
    source: &str,
) -> (u8, String) {
    stdin.write_all(&id.to_le_bytes()).unwrap();
    stdin
        .write_all(&(source.len() as u32).to_le_bytes())
        .unwrap();
    stdin.write_all(source.as_bytes()).unwrap();
    stdin.flush().unwrap();

    let mut id_bytes = [0_u8; 8];
    stdout.read_exact(&mut id_bytes).unwrap();
    assert_eq!(u64::from_le_bytes(id_bytes), id);
    let mut status = [0_u8; 1];
    stdout.read_exact(&mut status).unwrap();
    let mut length = [0_u8; 4];
    stdout.read_exact(&mut length).unwrap();
    let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
    stdout.read_exact(&mut payload).unwrap();
    (status[0], String::from_utf8(payload).unwrap())
}

#[test]
fn real_worker_rejects_configuration_then_renders_a_safe_svg() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_native-markdown"))
        .arg("--native-markdown-mermaid-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    let (status, error) = send_request(
        &mut stdin,
        &mut stdout,
        1,
        "%%{init: {'theme': 'dark'}}%%\nflowchart LR\nA-->B",
    );
    assert_eq!(status, 1);
    assert!(error.contains("disabled"));

    let (status, svg) = send_request(
        &mut stdin,
        &mut stdout,
        2,
        "flowchart LR\nA[开始] --> B[完成]",
    );
    assert_eq!(status, 0, "{svg}");
    assert!(svg.starts_with("<svg"), "{svg}");
    assert!(svg.contains("开始"), "{svg}");
    assert!(!svg.to_ascii_lowercase().contains("<foreignobject"));
    assert!(!svg.to_ascii_lowercase().contains("<script"));

    let (status, svg) = send_request(
        &mut stdin,
        &mut stdout,
        3,
        "flowchart LR\nA[一<br>二] --> B[三<BR/>四] --> C[五<br />六]",
    );
    assert_eq!(status, 0, "{svg}");
    let lowercase_svg = svg.to_ascii_lowercase();
    assert!(!lowercase_svg.contains("<foreignobject"));
    assert!(!lowercase_svg.contains("<script"));
    assert!(!lowercase_svg.contains("<br"));

    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn real_manager_starts_a_constrained_worker_and_completes_rendering() {
    let output = Command::new(env!("CARGO_BIN_EXE_native-markdown"))
        .arg("--native-markdown-mermaid-self-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("result=pass"));
}
