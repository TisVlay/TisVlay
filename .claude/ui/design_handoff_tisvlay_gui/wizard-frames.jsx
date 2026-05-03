// Pairing wizard frames — each `Frame*` renders one annotated state.
// Slint-1.16-friendly: only stock widgets, simple animations, modal sizing.

const SAMPLE_TVS = [
  { id: "lr",  vendor: "lg",      name: "Living Room G4", model: "OLED65G4", ip: "192.168.1.100", timestamp: "discovered just now" },
  { id: "office", vendor: "samsung", name: "Office QN90C",   model: "QN65QN90C", ip: "192.168.1.142", timestamp: "1 second ago" },
  { id: "guest", vendor: "sony",   name: "Guest Bravia",    model: "XR-65A95L", ip: "192.168.1.187", timestamp: "fading in…" },
];

// =========================================================================
// STEP 1 — Discover
// =========================================================================

function FrameDiscoverStreaming({ theme }) {
  const t = T[theme];
  const [selected, setSelected] = React.useState("lr");
  const [manualOpen, setManualOpen] = React.useState(false);
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={1} />
      <Body theme={theme}>
        <SectionHeading
          theme={theme}
          title="Find your TV"
          subtitle="Searching the local network for compatible TVs."
        />
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <PulseDot theme={theme} />
          <span style={{ fontSize: 12, color: t.textDim }}>
            Scanning via SSDP — found 2, listening for more…
          </span>
        </div>
        <div style={{
          border: `1px solid ${t.border}`, borderRadius: 5, padding: 4,
          background: t.surface, marginBottom: 12,
        }}>
          <DiscoveryRow theme={theme} tv={SAMPLE_TVS[0]} selected={selected === "lr"} onClick={() => setSelected("lr")} />
          <DiscoveryRow theme={theme} tv={SAMPLE_TVS[1]} selected={selected === "office"} onClick={() => setSelected("office")} />
          <DiscoveryRow theme={theme} tv={SAMPLE_TVS[2]} selected={false} fadingIn onClick={() => {}} />
          <div style={{ padding: "8px 12px 6px" }}>
            <ScanLine theme={theme} />
          </div>
        </div>
        <Collapsible
          theme={theme}
          title="Don't see your TV? Add it manually"
          open={manualOpen}
          onToggle={setManualOpen}
        >
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr auto", gap: 10, paddingTop: 12 }}>
            <FormRow theme={theme} label="Vendor">
              <Select theme={theme} value="lg" options={[
                { value: "lg", label: "LG WebOS" },
                { value: "samsung", label: "Samsung" },
                { value: "sony", label: "Sony" },
                { value: "mock", label: "Mock (testing)" },
              ]} />
            </FormRow>
            <FormRow theme={theme} label="IP address" helper="SSDP blocked? Type the LAN IP.">
              <TextInput theme={theme} placeholder="192.168.1.100" monospace />
            </FormRow>
            <div style={{ display: "flex", alignItems: "flex-end" }}>
              <Btn theme={theme}>Add manually</Btn>
            </div>
          </div>
        </Collapsible>
      </Body>
      <Footer theme={theme} hideBack nextDisabled={false} />
    </WindowChrome>
  );
}

function FrameDiscoverEmpty({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={1} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Find your TV" subtitle="Searching the local network for compatible TVs." />
        <div style={{
          border: `1px dashed ${t.borderStrong}`,
          borderRadius: 6, padding: "32px 20px",
          display: "flex", flexDirection: "column", alignItems: "center", gap: 14,
          background: t.surface2,
        }}>
          <div style={{ position: "relative", width: 56, height: 56 }}>
            <Spinner theme={theme} size={56} />
            <div style={{
              position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center",
              color: t.accent, fontSize: 22,
            }}>
              <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
                <rect x="3" y="5" width="18" height="12" rx="1.5" stroke="currentColor" strokeWidth="1.6" />
                <path d="M8 21h8M12 17v4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
              </svg>
            </div>
          </div>
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: t.text }}>Searching your network…</div>
            <div style={{ fontSize: 12, color: t.textDim, marginTop: 4 }}>
              Make sure the TV is on the same Wi-Fi or LAN.
            </div>
          </div>
          <div style={{ width: 240 }}>
            <ScanLine theme={theme} />
          </div>
        </div>
        <div style={{ marginTop: 14 }}>
          <Collapsible theme={theme} title="Don't see your TV? Add it manually" defaultOpen={false}>
            <div style={{ paddingTop: 10, fontSize: 12, color: t.textDim }}>
              SSDP blocked on your router or VLAN? Add the TV by its LAN IP.
            </div>
          </Collapsible>
        </div>
      </Body>
      <Footer theme={theme} hideBack nextDisabled />
    </WindowChrome>
  );
}

function FrameDiscoverError({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={1} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Find your TV" subtitle="Searching the local network for compatible TVs." />
        <div style={{
          background: t.dangerSoft,
          border: `1px solid ${t.danger}`,
          borderRadius: 5,
          padding: "10px 12px",
          display: "flex", gap: 10, marginBottom: 14,
        }}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" style={{ flexShrink: 0, marginTop: 1 }}>
            <circle cx="8" cy="8" r="7" stroke={t.danger} strokeWidth="1.5" />
            <path d="M8 4.5v4M8 11v.5" stroke={t.danger} strokeWidth="1.5" strokeLinecap="round" />
          </svg>
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, fontWeight: 600, color: t.danger }}>Discovery failed</div>
            <div style={{ fontSize: 12, color: t.text, marginTop: 2 }}>
              Could not bind UDP socket on port 1900 (SSDP). Another service may be using it,
              or your firewall is blocking multicast.
            </div>
            <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
              <Btn theme={theme} size="sm">Retry scan</Btn>
              <Btn theme={theme} size="sm">View troubleshooting</Btn>
            </div>
          </div>
        </div>
        <Collapsible theme={theme} title="Don't see your TV? Add it manually" defaultOpen>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr auto", gap: 10, paddingTop: 12 }}>
            <FormRow theme={theme} label="Vendor">
              <Select theme={theme} value="lg" options={[
                { value: "lg", label: "LG WebOS" },
                { value: "samsung", label: "Samsung" },
                { value: "sony", label: "Sony" },
                { value: "mock", label: "Mock (testing)" },
              ]} />
            </FormRow>
            <FormRow theme={theme} label="IP address" helper="Use the LAN IP.">
              <TextInput theme={theme} value="192.168.1.100" monospace />
            </FormRow>
            <div style={{ display: "flex", alignItems: "flex-end" }}>
              <Btn theme={theme} variant="accent">Add manually</Btn>
            </div>
          </div>
        </Collapsible>
      </Body>
      <Footer theme={theme} hideBack nextDisabled />
    </WindowChrome>
  );
}

// =========================================================================
// STEP 2 — Configure
// =========================================================================

function FrameConfigure({ theme }) {
  const t = T[theme];
  const [name, setName] = React.useState("Living Room G4");
  const [profile, setProfile] = React.useState("default");
  const [managed, setManaged] = React.useState(true);
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={2} />
      <Body theme={theme}>
        <SectionHeading
          theme={theme}
          title="Configure this TV"
          subtitle="Set how tisvlay should refer to and automate this device."
        />
        <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
          <FormRow theme={theme} label="Friendly name" helper="Shown in the TVs pane, tray menu, and CLI.">
            <TextInput theme={theme} value={name} onChange={setName} />
          </FormRow>

          <FormRow theme={theme} label="Vendor">
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <VendorIcon theme={theme} vendor="lg" size={24} />
              <Chip theme={theme}>LG WebOS</Chip>
              <span style={{ fontSize: 11.5, color: t.textFaint, fontFamily: "ui-monospace, monospace" }}>
                192.168.1.100 · OLED65G4
              </span>
            </div>
          </FormRow>

          <FormRow
            theme={theme}
            label="Initial system profile"
            helper="System profiles drive boot/shutdown/sleep behavior. You can change this later."
          >
            <Select theme={theme} value={profile} onChange={setProfile} options={[
              { value: "default",     label: "default — power on at boot, off at shutdown" },
              { value: "movie-night", label: "movie-night — never auto-power-off" },
              { value: "gaming",      label: "gaming — manual control, no session events" },
              { value: "guest",       label: "guest — screen-off on shutdown" },
            ]} />
          </FormRow>

          <div style={{
            border: `1px solid ${t.border}`,
            borderRadius: 5,
            padding: "12px 14px",
            background: t.surface2,
          }}>
            <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
              <Checkbox
                theme={theme}
                checked={managed}
                onChange={() => setManaged(!managed)}
                label="Managed by tisvlay"
                helper="The daemon will power-cycle this TV in response to desktop session events (lock, unlock, sleep, shutdown). Untick to keep it paired but ignored by automation."
              />
            </div>
          </div>
        </div>
      </Body>
      <Footer theme={theme} />
    </WindowChrome>
  );
}

// =========================================================================
// STEP 3 — Pair (4 sub-states)
// =========================================================================

function PairCard({ theme, children }) {
  const t = T[theme];
  return (
    <div style={{
      border: `1px solid ${t.border}`,
      borderRadius: 6,
      background: t.surface2,
      padding: "28px 24px",
      display: "flex", flexDirection: "column", alignItems: "center", gap: 18,
      minHeight: 260,
      justifyContent: "center",
      textAlign: "center",
    }}>
      {children}
    </div>
  );
}

function FramePairPrompting({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={3} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Pair with the TV" subtitle="One-time handshake to authorize tisvlay." />
        <PairCard theme={theme}>
          <TvIllustration theme={theme} prompt />
          <div>
            <div style={{ fontSize: 14, fontWeight: 600, color: t.text }}>
              Confirm the prompt on your TV
            </div>
            <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 4, maxWidth: 360 }}>
              Use the magic remote (or the directional pad) to choose <b>Accept</b>.
              The prompt times out after 60 seconds.
            </div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <PulseDot theme={theme} />
            <span style={{ fontSize: 11.5, color: t.textFaint }}>Waiting for response…</span>
          </div>
        </PairCard>
      </Body>
      <Footer theme={theme} hideBack nextLabel="Next" nextDisabled />
    </WindowChrome>
  );
}

function FramePairRegistering({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={3} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Pair with the TV" subtitle="One-time handshake to authorize tisvlay." />
        <PairCard theme={theme}>
          <Spinner theme={theme} size={48} />
          <div>
            <div style={{ fontSize: 14, fontWeight: 600, color: t.text }}>Registering with the TV…</div>
            <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 4 }}>
              Exchanging client key over <span style={{ fontFamily: "ui-monospace, monospace" }}>wss://192.168.1.100:3001</span>.
            </div>
          </div>
          <div style={{ width: 220 }}>
            <ScanLine theme={theme} />
          </div>
        </PairCard>
      </Body>
      <Footer theme={theme} hideBack nextLabel="Next" nextDisabled />
    </WindowChrome>
  );
}

function FramePairComplete({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={3} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Pair with the TV" subtitle="One-time handshake to authorize tisvlay." />
        <PairCard theme={theme}>
          <div style={{
            width: 56, height: 56, borderRadius: 28,
            background: t.successSoft, color: t.success,
            display: "flex", alignItems: "center", justifyContent: "center",
            border: `1px solid ${t.success}`,
          }}>
            <svg width="28" height="28" viewBox="0 0 24 24" fill="none">
              <path d="M5 12.5L10 17.5L19 7.5" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </div>
          <div>
            <div style={{ fontSize: 15, fontWeight: 600, color: t.text }}>Paired successfully</div>
            <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 4 }}>
              Client key stored. tisvlay can now control this TV.
            </div>
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6, fontSize: 12, color: t.textDim, alignItems: "flex-start" }}>
            <div style={{ display: "flex", gap: 6 }}>
              <span style={{ color: t.success }}>✓</span> WebSocket session established
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <span style={{ color: t.success }}>✓</span> Capabilities probed (POWER · SCREEN · VOLUME · INPUT · BACKLIGHT)
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <span style={{ color: t.success }}>✓</span> WoL MAC <span style={{ fontFamily: "ui-monospace, monospace" }}>AA:BB:CC:DD:EE:FF</span> recorded
            </div>
          </div>
        </PairCard>
      </Body>
      <Footer theme={theme} nextLabel="Next" />
    </WindowChrome>
  );
}

function FramePairFailed({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={3} />
      <Body theme={theme}>
        <SectionHeading theme={theme} title="Pair with the TV" subtitle="One-time handshake to authorize tisvlay." />
        <PairCard theme={theme}>
          <div style={{
            width: 56, height: 56, borderRadius: 28,
            background: t.dangerSoft, color: t.danger,
            display: "flex", alignItems: "center", justifyContent: "center",
            border: `1px solid ${t.danger}`,
          }}>
            <svg width="28" height="28" viewBox="0 0 24 24" fill="none">
              <path d="M7 7L17 17M17 7L7 17" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
            </svg>
          </div>
          <div>
            <div style={{ fontSize: 15, fontWeight: 600, color: t.text }}>Pairing failed</div>
            <div style={{ fontSize: 12.5, color: t.danger, marginTop: 6, maxWidth: 380 }}>
              The prompt was rejected on the TV, or the request timed out after 60 seconds.
            </div>
            <div style={{
              marginTop: 10,
              padding: "8px 10px",
              background: t.surface,
              border: `1px solid ${t.border}`,
              borderRadius: 4,
              fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
              fontSize: 11,
              color: t.textDim,
              maxWidth: 380,
              textAlign: "left",
            }}>
              ssap://api/getServiceList → 401 rejectedRegistration
            </div>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <Btn theme={theme} variant="accent">Retry pairing</Btn>
            <Btn theme={theme}>Back to discovery</Btn>
          </div>
        </PairCard>
      </Body>
      <Footer theme={theme} hideCancel hideBack nextLabel="Next" nextDisabled />
    </WindowChrome>
  );
}

// =========================================================================
// STEP 4 — Done
// =========================================================================

function FrameDone({ theme }) {
  const t = T[theme];
  return (
    <WindowChrome theme={theme} title="Pair a new TV — tisvlay">
      <Stepper theme={theme} current={4} />
      <Body theme={theme}>
        <div style={{
          flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center",
          gap: 18, padding: "24px 0", textAlign: "center", minHeight: 320,
        }}>
          <div style={{
            width: 64, height: 64, borderRadius: 32,
            background: t.successSoft, color: t.success,
            display: "flex", alignItems: "center", justifyContent: "center",
            border: `1px solid ${t.success}`,
          }}>
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none">
              <path d="M5 12.5L10 17.5L19 7.5" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </div>
          <div>
            <div style={{ fontSize: 18, fontWeight: 600, color: t.text }}>
              Living Room G4 is ready.
            </div>
            <div style={{ fontSize: 13, color: t.textDim, marginTop: 6, maxWidth: 380 }}>
              The new TV will appear selected in the TVs pane. Boot, sleep, and session
              events will follow the <b>default</b> system profile until you change it.
            </div>
          </div>
          <div style={{
            marginTop: 4,
            display: "flex", gap: 14, fontSize: 11.5, color: t.textFaint,
            alignItems: "center",
          }}>
            <span>Vendor: <b style={{ color: t.textDim }}>LG WebOS</b></span>
            <span>·</span>
            <span style={{ fontFamily: "ui-monospace, monospace" }}>192.168.1.100</span>
            <span>·</span>
            <span>Managed</span>
          </div>
        </div>
      </Body>
      <Footer
        theme={theme}
        hideCancel hideBack
        nextLabel="Close"
        primaryStyle="accent"
      />
    </WindowChrome>
  );
}

Object.assign(window, {
  FrameDiscoverStreaming, FrameDiscoverEmpty, FrameDiscoverError,
  FrameConfigure,
  FramePairPrompting, FramePairRegistering, FramePairComplete, FramePairFailed,
  FrameDone,
});
