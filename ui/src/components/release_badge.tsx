interface Release_badge_props {
  version: string;
  channel: 'demo' | 'development' | 'release';
  label: string;
  description: string;
  platform?: string;
  architecture?: string;
}

export function Release_badge({ version, channel, label, description, platform, architecture }: Release_badge_props) {
  const target_label = [platform, architecture].filter(Boolean).join(' / ');
  const title = target_label ? `${description} · ${target_label}` : description;
  return (
    <span className="release_badge">
      <span className="release_version" data-testid="build_version">v{version}</span>
      <span
        className="release_channel"
        data-channel={channel}
        data-testid="build_channel"
        title={title}
      >
        {label}
      </span>
    </span>
  );
}
