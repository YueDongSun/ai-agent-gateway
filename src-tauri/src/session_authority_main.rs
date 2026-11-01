fn main() {
    let code =
        ai_agent_gateway_lib::services::session_authority::main(std::env::args_os().collect());
    std::process::exit(code);
}
