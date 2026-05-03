// Static gallery: shows all panes side-by-side in light + dark using design canvas.

function Gallery() {
  return (
    <DesignCanvas title="tisvlay GUI · gallery">
      <DCSection id="overview" title="Overview" subtitle="Sidebar navigation between TVs / Profiles / Session / About. Same shell in light + dark.">
        <DCArtboard id="tvs-light" label="TVs · light" width={1040} height={680}>
          <ShellFrame theme="light" pane="tvs" />
        </DCArtboard>
        <DCArtboard id="tvs-dark" label="TVs · dark" width={1040} height={680}>
          <ShellFrame theme="dark" pane="tvs" />
        </DCArtboard>
      </DCSection>

      <DCSection id="tvs" title="TVs pane" subtitle="Master/detail. List on the left with a + button to launch the pairing wizard; detail panel groups Power, Audio, Picture, Input, Configuration, Hardware, and Danger zone.">
        <DCArtboard id="tvs-detail-light" label="Detail · selected · light" width={1040} height={680}>
          <ShellFrame theme="light" pane="tvs" selectedId="living-room" />
        </DCArtboard>
        <DCArtboard id="tvs-detail-dark" label="Detail · selected · dark" width={1040} height={680}>
          <ShellFrame theme="dark" pane="tvs" selectedId="living-room" />
        </DCArtboard>
        <DCArtboard id="tvs-disconnected" label="Detail · disconnected TV" width={1040} height={680}>
          <ShellFrame theme="light" pane="tvs" selectedId="guest" />
        </DCArtboard>
        <DCArtboard id="tvs-empty" label="Empty · no TVs paired" width={1040} height={680}>
          <ShellFrame theme="light" pane="tvs" tvsOverride={[]} />
        </DCArtboard>
        <DCArtboard id="tvs-offline" label="Daemon offline" width={1040} height={680}>
          <ShellFrame theme="light" pane="tvs" daemonAvailable={false} />
        </DCArtboard>
      </DCSection>

      <DCSection id="profiles" title="Profiles pane" subtitle="System + User tabs. Master list, then editor with explicit inheritance from `default`. Empty fields show a `↳ inherits` hint.">
        <DCArtboard id="profiles-system-default" label="System · default · light" width={1040} height={680}>
          <ShellFrame theme="light" pane="profiles" />
        </DCArtboard>
        <DCArtboard id="profiles-system-default-dark" label="System · default · dark" width={1040} height={680}>
          <ShellFrame theme="dark" pane="profiles" />
        </DCArtboard>
      </DCSection>

      <DCSection id="about" title="About pane" subtitle="Version, daemon + session connection status, component map, license.">
        <DCArtboard id="about-light" label="About · light" width={1040} height={680}>
          <ShellFrame theme="light" pane="about" />
        </DCArtboard>
        <DCArtboard id="about-dark" label="About · dark" width={1040} height={680}>
          <ShellFrame theme="dark" pane="about" />
        </DCArtboard>
      </DCSection>
    </DesignCanvas>
  );
}

// Static frame — replicates the AppShell without state, used inside artboards.
function ShellFrame({ theme, pane, selectedId = "living-room", tvsOverride, daemonAvailable = true }) {
  const tvs = tvsOverride || SAMPLE_TVS_FULL;
  const t = APP_T[theme];

  let paneEl;
  if (!daemonAvailable) {
    paneEl = (
      <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: t.surface }}>
        <PaneHeader theme={theme} title="TVs" subtitle="Daemon unreachable." />
        <TvsPaneOffline theme={theme} />
      </div>
    );
  } else if (pane === "tvs") {
    paneEl = (
      <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: t.surface }}>
        <PaneHeader theme={theme}
          title="TVs"
          subtitle={tvs.length === 0 ? "No TVs configured" : `${tvs.length} configured · ${tvs.filter(x => x.connected).length} connected`}
          actions={tvs.length > 0 && (
            <AppBtn theme={theme} variant="accent"
              iconLeft={<SidebarIcon name="plus" size={14} />}>
              Pair new TV
            </AppBtn>
          )} />
        <TvsPane theme={theme} tvs={tvs} selectedId={selectedId}
          onSelect={() => {}} onTvChange={() => {}} onAdd={() => {}} />
      </div>
    );
  } else if (pane === "profiles") {
    paneEl = <ProfilesPane theme={theme} />;
  } else if (pane === "about") {
    paneEl = <AboutPane theme={theme} daemonAvailable={true} sessionAvailable={true} />;
  }

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", alignItems: "stretch", justifyContent: "stretch", background: theme === "light" ? "#e8ebf0" : "#0e1014" }}>
      <AppWindow theme={theme} width="100%" height="100%" title="tisvlay">
        <Sidebar theme={theme} active={pane}
          daemonAvailable={daemonAvailable} sessionAvailable={true} />
        {paneEl}
      </AppWindow>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<Gallery />);
