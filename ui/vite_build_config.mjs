export function build_settings(command, mode) {
  const is_demo_build = mode === 'demo';
  const build_channel = command === 'serve'
    ? (is_demo_build ? 'demo' : 'development')
    : (is_demo_build ? 'demo' : 'release');

  return {
    build_channel,
    is_demo_build,
    out_dir: is_demo_build ? 'dist_demo' : 'dist',
  };
}
