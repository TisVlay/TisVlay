// Interactive prototype: full tisvlay GUI window with sidebar nav.

const TWEAK_DEFAULTS_APP = /*EDITMODE-BEGIN*/{
  "theme": "light"
}/*EDITMODE-END*/;

function App() {
  const { tweaks, setTweak } = useTweaks(TWEAK_DEFAULTS_APP);
  const theme = (tweaks && tweaks.theme) || "light";
  const t = APP_T[theme];
  const [pane, setPane] = React.useState("tvs");
  const [tvs, setTvs] = React.useState(SAMPLE_TVS_FULL);
  const [selectedId, setSelectedId] = React.useState("living-room");
  const [showWizard, setShowWizard] = React.useState(false);
  const [daemonAvailable, setDaemonAvailable] = React.useState(true);

  React.useEffect(() => {
    document.body.style.background = theme === "light" ? "#e8ebf0" : "#0e1014";
  }, [theme]);

  const updateTv = (next) => {
    setTvs(tvs.map((tv) => tv.id === next.id ? next : tv));
  };

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
          title="TVs" subtitle={`${tvs.length} configured · ${tvs.filter(x => x.connected).length} connected`}
          actions={
            <AppBtn theme={theme} variant="accent" size="md"
              iconLeft={<SidebarIcon name="plus" size={14} />}
              onClick={() => setShowWizard(true)}>Pair new TV</AppBtn>
          } />
        <TvsPane theme={theme} tvs={tvs} selectedId={selectedId}
          onSelect={setSelectedId} onTvChange={updateTv}
          onAdd={() => setShowWizard(true)} />
      </div>
    );
  } else if (pane === "profiles") {
    paneEl = <ProfilesPane theme={theme} />;
  } else if (pane === "session") {
    paneEl = <SessionPane theme={theme} />;
  } else {
    paneEl = <AboutPane theme={theme} daemonAvailable={daemonAvailable} sessionAvailable={true} />;
  }

  return (
    <div style={{ minHeight: "100vh", display: "flex", alignItems: "center", justifyContent: "center", padding: 24 }}>
      <AppWindow theme={theme} width={1040} height={680} title="tisvlay">
        <Sidebar theme={theme} active={pane} onSelect={setPane}
          daemonAvailable={daemonAvailable} sessionAvailable={true} />
        {paneEl}
      </AppWindow>

      {showWizard && (
        <div style={{
          position: "fixed", inset: 0, background: "rgba(10,12,16,0.5)",
          display: "flex", alignItems: "center", justifyContent: "center", zIndex: 50,
        }} onClick={() => setShowWizard(false)}>
          <div onClick={(e) => e.stopPropagation()}>
            <WizardModal theme={theme} onClose={() => setShowWizard(false)}
              onComplete={() => setShowWizard(false)} />
          </div>
        </div>
      )}

      <TweaksPanel title="Tweaks">
        <TweakSection title="Theme">
          <TweakRadio label="Mode" value={theme}
            options={[{ value: "light", label: "Light" }, { value: "dark", label: "Dark" }]}
            onChange={(v) => setTweak("theme", v)} />
        </TweakSection>
        <TweakSection title="Demo state">
          <TweakToggle label="Daemon online" value={daemonAvailable}
            onChange={(v) => setDaemonAvailable(v)} />
          <TweakButton onClick={() => setShowWizard(true)}>Open pairing wizard</TweakButton>
          <TweakSelect label="Active pane" value={pane}
            options={[
              { value: "tvs", label: "TVs" },
              { value: "profiles", label: "Profiles" },
              { value: "session", label: "Session" },
              { value: "about", label: "About" },
            ]}
            onChange={(v) => setPane(v)} />
        </TweakSection>
      </TweaksPanel>
    </div>
  );
}

// Wizard wrapper — uses the existing wizard frames in `pair-prompting` mode by default.
// The Pairing Wizard.html owns the rich frame variants; here we embed a minimal
// step-driven flow for the prototype.
function WizardModal({ theme, onClose, onComplete }) {
  const [step, setStep] = React.useState(1);
  const [selected, setSelected] = React.useState(null);
  const [config, setConfig] = React.useState({
    name: "Living Room", systemProfile: "default", managed: true,
  });

  return (
    <div style={{
      width: 600, height: 540, background: APP_T[theme].surface,
      borderRadius: 8, boxShadow: APP_T[theme].shadow,
      display: "flex", flexDirection: "column", overflow: "hidden",
      fontFamily: APP_FONT, color: APP_T[theme].text,
      border: `1px solid ${APP_T[theme].border}`,
    }}>
      <div style={{
        padding: "14px 20px", borderBottom: `1px solid ${APP_T[theme].border}`,
        display: "flex", alignItems: "center", gap: 12,
      }}>
        <div style={{ fontSize: 15, fontWeight: 600, flex: 1 }}>Pair a new TV</div>
        <Stepper theme={theme} step={step} />
        <button onClick={onClose} style={{
          width: 28, height: 28, border: "none", background: "transparent",
          color: APP_T[theme].textDim, cursor: "pointer", fontSize: 18,
        }}>×</button>
      </div>
      <div style={{ flex: 1, padding: 24, overflow: "auto" }}>
        {step === 1 && <WizardStep1 theme={theme} selected={selected} onSelect={setSelected} />}
        {step === 2 && <WizardStep2 theme={theme} selected={selected} config={config} onChange={setConfig} />}
        {step === 3 && <WizardStep3 theme={theme} />}
        {step === 4 && <WizardStep4 theme={theme} name={config.name} />}
      </div>
      <div style={{
        padding: "12px 20px", borderTop: `1px solid ${APP_T[theme].border}`,
        display: "flex", justifyContent: "space-between",
      }}>
        <AppBtn theme={theme} onClick={onClose}>Cancel</AppBtn>
        <div style={{ display: "flex", gap: 8 }}>
          {step > 1 && step < 4 && <AppBtn theme={theme} onClick={() => setStep(step - 1)}>Back</AppBtn>}
          {step < 4 && (
            <AppBtn theme={theme} variant="accent"
              disabled={step === 1 && !selected}
              onClick={() => setStep(step + 1)}>
              {step === 3 ? "Continue" : "Next"}
            </AppBtn>
          )}
          {step === 4 && <AppBtn theme={theme} variant="accent" onClick={onComplete}>Done</AppBtn>}
        </div>
      </div>
    </div>
  );
}

function Stepper({ theme, step }) {
  const t = APP_T[theme];
  const labels = ["Discover", "Configure", "Pair", "Done"];
  return (
    <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
      {labels.map((label, i) => {
        const n = i + 1;
        const active = n === step;
        const done = n < step;
        return (
          <React.Fragment key={n}>
            <div style={{
              display: "inline-flex", alignItems: "center", gap: 5,
              padding: "3px 8px", borderRadius: 10,
              background: active ? t.accent : (done ? t.successSoft : t.chip),
              color: active ? "#fff" : (done ? t.successText : t.chipText),
              fontSize: 11, fontWeight: 600,
            }}>
              <span>{done ? "✓" : n}</span>
              <span>{label}</span>
            </div>
            {i < labels.length - 1 && <span style={{ color: t.textFaint, fontSize: 10 }}>›</span>}
          </React.Fragment>
        );
      })}
    </div>
  );
}

function WizardStep1({ theme, selected, onSelect }) {
  const t = APP_T[theme];
  const found = [
    { id: "lg-1", name: "[LG] webOS TV OLED65G4", vendor: "lg", ip: "192.168.1.100", model: "OLED65G4PUA" },
    { id: "ss-1", name: "[Samsung] QN65QN90C", vendor: "samsung", ip: "192.168.1.142", model: "QN65QN90C" },
  ];
  return (
    <div>
      <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 4 }}>Discovering TVs on your network…</div>
      <div style={{ fontSize: 12.5, color: t.textDim, marginBottom: 16 }}>
        SSDP scan in progress. Make sure your TV is powered on and on the same network.
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {found.map((tv) => (
          <div key={tv.id} onClick={() => onSelect(tv)} style={{
            padding: "12px 14px", borderRadius: 5,
            border: `1.5px solid ${selected?.id === tv.id ? t.accent : t.border}`,
            background: selected?.id === tv.id ? t.accentSoft : t.surface,
            display: "flex", alignItems: "center", gap: 12, cursor: "pointer",
          }}>
            <AppVendorIcon theme={theme} vendor={tv.vendor} size={32} />
            <div style={{ flex: 1 }}>
              <div style={{ fontSize: 13, fontWeight: 500 }}>{tv.name}</div>
              <div style={{ fontSize: 11.5, color: t.textDim, marginTop: 2 }}>
                <span style={{ fontFamily: "ui-monospace, monospace" }}>{tv.ip}</span> · {tv.model}
              </div>
            </div>
            {selected?.id === tv.id && (
              <svg width="20" height="20" viewBox="0 0 12 12" fill="none">
                <circle cx="6" cy="6" r="5.5" fill={t.accent} />
                <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="white" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            )}
          </div>
        ))}
        <div style={{
          padding: "10px 14px", borderRadius: 5,
          background: t.surface2, color: t.textDim, fontSize: 12,
          display: "flex", alignItems: "center", gap: 10,
        }}>
          <span style={{
            width: 10, height: 10, borderRadius: 5,
            background: t.accent, animation: "tv-pulse 1.4s infinite",
          }} />
          Still scanning…
        </div>
      </div>
    </div>
  );
}

function WizardStep2({ theme, selected, config, onChange }) {
  const t = APP_T[theme];
  return (
    <div>
      <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 4 }}>Configure {selected?.name || "TV"}</div>
      <div style={{ fontSize: 12.5, color: t.textDim, marginBottom: 16 }}>
        Settings can be changed later from the TVs pane.
      </div>
      <AppFormRow theme={theme} label="Friendly name"
        helper="Shown throughout the app and CLI.">
        <AppText theme={theme} value={config.name}
          onChange={(v) => onChange({ ...config, name: v })} />
      </AppFormRow>
      <div style={{ height: 12 }} />
      <AppFormRow theme={theme} label="Vendor">
        <div style={{ display: "flex", gap: 8, alignItems: "center", height: 30 }}>
          <AppVendorIcon theme={theme} vendor={selected?.vendor || "mock"} size={22} />
          <span style={{ fontSize: 13, color: t.textDim, textTransform: "capitalize" }}>{selected?.vendor || "—"}</span>
          <span style={{ fontSize: 11, color: t.textFaint }}>(detected)</span>
        </div>
      </AppFormRow>
      <div style={{ height: 12 }} />
      <AppFormRow theme={theme} label="System profile"
        helper="Drives daemon lifecycle automation.">
        <AppSelect theme={theme} value={config.systemProfile}
          options={[
            { value: "default", label: "default" },
            { value: "guest", label: "guest" },
            { value: "shared", label: "shared" },
          ]}
          onChange={(v) => onChange({ ...config, systemProfile: v })} />
      </AppFormRow>
      <div style={{ height: 16 }} />
      <AppCheckbox theme={theme} checked={config.managed}
        onChange={(v) => onChange({ ...config, managed: v })}
        label="Managed by tisvlay"
        helper="Allow boot/shutdown/sleep automation. You can also flip this later." />
    </div>
  );
}

function WizardStep3({ theme }) {
  const t = APP_T[theme];
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", height: "100%", textAlign: "center" }}>
      <div style={{
        width: 64, height: 64, borderRadius: 32,
        background: t.successSoft, color: t.success,
        display: "flex", alignItems: "center", justifyContent: "center",
        marginBottom: 18,
      }}>
        <svg width="32" height="32" viewBox="0 0 24 24" fill="none">
          <path d="M5 12.5L10 17.5L19 7.5" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </div>
      <div style={{ fontSize: 17, fontWeight: 600 }}>Pairing successful</div>
      <div style={{ fontSize: 13, color: t.textDim, marginTop: 6, maxWidth: 380 }}>
        The TV accepted the registration request. Capabilities are loaded.
      </div>
    </div>
  );
}

function WizardStep4({ theme, name }) {
  const t = APP_T[theme];
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", height: "100%", textAlign: "center" }}>
      <div style={{
        width: 64, height: 64, borderRadius: 32,
        background: t.accentSoft, color: t.accent,
        display: "flex", alignItems: "center", justifyContent: "center",
        marginBottom: 18,
      }}>
        <SidebarIcon name="tv" size={32} />
      </div>
      <div style={{ fontSize: 17, fontWeight: 600 }}>{name} is ready</div>
      <div style={{ fontSize: 13, color: t.textDim, marginTop: 6, maxWidth: 380 }}>
        It's now selected in the TVs pane. Try toggling power, volume, and inputs.
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<App />);
Object.assign(window, { App, WizardModal });
