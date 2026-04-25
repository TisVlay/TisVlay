// Shared primitives for the tisvlay pairing wizard frames.
// Light + dark themes via data-theme on root. KDE Breeze-friendly,
// neutral enough to render on GNOME/XFCE.

const T = {
  light: {
    bg: "#fbfbfc",
    surface: "#ffffff",
    surface2: "#f4f5f7",
    border: "#dcdfe4",
    borderStrong: "#c2c7d0",
    text: "#1a1d22",
    textDim: "#5b626d",
    textFaint: "#8a909a",
    accent: "var(--accent, #3b82f6)",
    accentSoft: "color-mix(in oklab, var(--accent, #3b82f6) 12%, white)",
    accentText: "white",
    success: "#10b981",
    successSoft: "#d1fae5",
    danger: "#dc2626",
    dangerSoft: "#fee2e2",
    rowHover: "#f4f5f7",
    rowSelected: "color-mix(in oklab, var(--accent, #3b82f6) 14%, white)",
    rowSelectedBorder: "var(--accent, #3b82f6)",
    chip: "#eef0f3",
    chipText: "#374151",
    shadow: "0 1px 2px rgba(0,0,0,.04), 0 8px 24px rgba(0,0,0,.06)",
    titlebar: "#eaecef",
    titlebarBorder: "#d2d6dc",
  },
  dark: {
    bg: "#1b1e23",
    surface: "#232730",
    surface2: "#2b2f39",
    border: "#383d48",
    borderStrong: "#4a4f5b",
    text: "#eef0f3",
    textDim: "#a4abb6",
    textFaint: "#6f7682",
    accent: "var(--accent, #3daee9)",
    accentSoft: "color-mix(in oklab, var(--accent, #3daee9) 22%, #232730)",
    accentText: "white",
    success: "#34d399",
    successSoft: "color-mix(in oklab, #34d399 22%, #232730)",
    danger: "#f87171",
    dangerSoft: "color-mix(in oklab, #f87171 22%, #232730)",
    rowHover: "#2b2f39",
    rowSelected: "color-mix(in oklab, var(--accent, #3daee9) 22%, #232730)",
    rowSelectedBorder: "var(--accent, #3daee9)",
    chip: "#323744",
    chipText: "#cfd4dc",
    shadow: "0 1px 2px rgba(0,0,0,.4), 0 12px 32px rgba(0,0,0,.35)",
    titlebar: "#262a33",
    titlebarBorder: "#383d48",
  },
};

// ---- Window chrome ---------------------------------------------------------
function WindowChrome({ theme = "light", title = "Pair a new TV — tisvlay", children, width = 600, height = 520 }) {
  const t = T[theme];
  return (
    <div
      style={{
        width, height,
        background: t.surface,
        color: t.text,
        borderRadius: 6,
        boxShadow: t.shadow,
        overflow: "hidden",
        fontFamily: 'system-ui, "Noto Sans", "Segoe UI", Roboto, "Helvetica Neue", sans-serif',
        fontSize: 13,
        lineHeight: 1.45,
        display: "flex",
        flexDirection: "column",
        border: `1px solid ${t.border}`,
      }}
    >
      <div
        style={{
          height: 32,
          background: t.titlebar,
          borderBottom: `1px solid ${t.titlebarBorder}`,
          display: "flex",
          alignItems: "center",
          padding: "0 10px",
          gap: 8,
          flexShrink: 0,
        }}
      >
        <div style={{ display: "flex", gap: 6 }}>
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#e0635a" }} />
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#e0a23a" }} />
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#5cb85c" }} />
        </div>
        <div style={{ flex: 1, textAlign: "center", fontSize: 12, color: t.textDim }}>{title}</div>
        <div style={{ width: 42 }} />
      </div>
      {children}
    </div>
  );
}

// ---- Stepper ---------------------------------------------------------------
const STEPS = [
  { n: 1, label: "Discover" },
  { n: 2, label: "Configure" },
  { n: 3, label: "Pair" },
  { n: 4, label: "Done" },
];

function Stepper({ theme, current }) {
  const t = T[theme];
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 0,
        padding: "16px 20px 14px",
        borderBottom: `1px solid ${t.border}`,
        background: t.surface2,
        flexShrink: 0,
      }}
    >
      {STEPS.map((s, i) => {
        const done = s.n < current;
        const active = s.n === current;
        return (
          <React.Fragment key={s.n}>
            <div style={{ display: "flex", alignItems: "center", gap: 8, flexShrink: 0 }}>
              <div
                style={{
                  width: 22, height: 22, borderRadius: 11,
                  display: "flex", alignItems: "center", justifyContent: "center",
                  fontSize: 11, fontWeight: 600,
                  background: done ? t.accent : active ? t.accent : t.surface,
                  color: done || active ? t.accentText : t.textDim,
                  border: `1px solid ${done || active ? t.accent : t.borderStrong}`,
                  transition: "all .15s",
                }}
              >
                {done ? (
                  <svg width="11" height="11" viewBox="0 0 12 12" fill="none">
                    <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="white" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
                  </svg>
                ) : s.n}
              </div>
              <span
                style={{
                  fontSize: 12,
                  fontWeight: active ? 600 : 500,
                  color: active ? t.text : done ? t.textDim : t.textFaint,
                }}
              >
                {s.label}
              </span>
            </div>
            {i < STEPS.length - 1 && (
              <div
                style={{
                  flex: 1,
                  height: 1,
                  background: s.n < current ? t.accent : t.border,
                  margin: "0 12px",
                  minWidth: 16,
                }}
              />
            )}
          </React.Fragment>
        );
      })}
    </div>
  );
}

// ---- Footer ----------------------------------------------------------------
function Footer({ theme, onBack, onNext, onCancel, nextLabel = "Next", nextDisabled = false, backDisabled = false, hideBack = false, hideCancel = false, primaryStyle = "accent" }) {
  const t = T[theme];
  return (
    <div
      style={{
        display: "flex",
        gap: 8,
        padding: "12px 16px",
        borderTop: `1px solid ${t.border}`,
        background: t.surface2,
        flexShrink: 0,
      }}
    >
      {!hideCancel && <Btn theme={theme} onClick={onCancel}>Cancel</Btn>}
      <div style={{ flex: 1 }} />
      {!hideBack && <Btn theme={theme} onClick={onBack} disabled={backDisabled}>Back</Btn>}
      <Btn theme={theme} variant={primaryStyle} onClick={onNext} disabled={nextDisabled}>
        {nextLabel}
      </Btn>
    </div>
  );
}

// ---- Button ----------------------------------------------------------------
function Btn({ theme, children, onClick, variant = "default", disabled, size = "md", iconLeft }) {
  const t = T[theme];
  const isPrimary = variant === "accent";
  const isDanger = variant === "danger";
  const bg = disabled
    ? (theme === "light" ? "#eceef1" : "#2b2f39")
    : isPrimary ? t.accent
    : isDanger ? t.danger
    : t.surface;
  const fg = disabled
    ? t.textFaint
    : isPrimary || isDanger ? t.accentText
    : t.text;
  const border = disabled
    ? t.border
    : isPrimary ? t.accent
    : isDanger ? t.danger
    : t.borderStrong;
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        gap: 6,
        height: size === "sm" ? 26 : 30,
        padding: size === "sm" ? "0 10px" : "0 14px",
        borderRadius: 4,
        background: bg,
        color: fg,
        border: `1px solid ${border}`,
        fontSize: 12.5,
        fontWeight: isPrimary ? 600 : 500,
        cursor: disabled ? "not-allowed" : "pointer",
        fontFamily: "inherit",
      }}
    >
      {iconLeft}
      {children}
    </button>
  );
}

// ---- Vendor icons ----------------------------------------------------------
function VendorIcon({ vendor, size = 28, theme }) {
  const t = T[theme];
  const map = {
    lg:      { letters: "LG", bg: "#a50034", fg: "#fff" },
    samsung: { letters: "SS", bg: "#1428a0", fg: "#fff" },
    sony:    { letters: "SY", bg: "#111111", fg: "#fff" },
    mock:    { letters: "MK", bg: t.chip,    fg: t.chipText },
  };
  const v = map[vendor] || map.mock;
  return (
    <div
      style={{
        width: size, height: size,
        borderRadius: 6,
        background: v.bg,
        color: v.fg,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        fontSize: size * 0.36,
        fontWeight: 700,
        letterSpacing: 0.5,
        flexShrink: 0,
        border: vendor === "mock" ? `1px solid ${t.border}` : "none",
        fontFamily: "inherit",
      }}
    >
      {v.letters}
    </div>
  );
}

// ---- Discovery row ---------------------------------------------------------
function DiscoveryRow({ theme, tv, selected, onClick, fadingIn }) {
  const t = T[theme];
  return (
    <div
      onClick={onClick}
      role="button"
      tabIndex={0}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "10px 12px",
        background: selected ? t.rowSelected : "transparent",
        border: `1px solid ${selected ? t.rowSelectedBorder : "transparent"}`,
        borderRadius: 5,
        cursor: "pointer",
        transition: "all .15s",
        opacity: fadingIn ? 0.55 : 1,
        transform: fadingIn ? "translateY(2px)" : "none",
      }}
    >
      <VendorIcon theme={theme} vendor={tv.vendor} />
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 8 }}>
          <span style={{ fontWeight: 600, color: t.text, fontSize: 13 }}>{tv.name}</span>
          <span style={{ color: t.textFaint, fontSize: 11 }}>{tv.model}</span>
        </div>
        <div style={{ color: t.textDim, fontSize: 11.5, fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace" }}>
          {tv.ip}
        </div>
      </div>
      <span style={{ color: t.textFaint, fontSize: 11 }}>{tv.timestamp}</span>
      {selected && (
        <div style={{
          width: 16, height: 16, borderRadius: 8, background: t.accent,
          display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
        }}>
          <svg width="9" height="9" viewBox="0 0 12 12" fill="none">
            <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="white" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </div>
      )}
    </div>
  );
}

// ---- Pulse / spinner -------------------------------------------------------
function PulseDot({ theme, color }) {
  const t = T[theme];
  const c = color || t.accent;
  return (
    <span style={{ position: "relative", width: 10, height: 10, display: "inline-block" }}>
      <span style={{
        position: "absolute", inset: 0, borderRadius: 5, background: c,
        opacity: 0.35, animation: "tv-pulse 1.4s ease-out infinite",
      }} />
      <span style={{
        position: "absolute", inset: 2, borderRadius: 3, background: c,
      }} />
    </span>
  );
}

function Spinner({ theme, size = 28, color }) {
  const t = T[theme];
  const c = color || t.accent;
  return (
    <div style={{
      width: size, height: size, borderRadius: size / 2,
      border: `2.5px solid ${theme === "light" ? "#e1e4ea" : "#383d48"}`,
      borderTopColor: c,
      animation: "tv-spin 0.9s linear infinite",
    }} />
  );
}

// ---- Form row --------------------------------------------------------------
function FormRow({ theme, label, helper, children, error }) {
  const t = T[theme];
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <label style={{ fontSize: 12, fontWeight: 500, color: t.text }}>{label}</label>
      {children}
      {error && <div style={{ fontSize: 11.5, color: t.danger }}>{error}</div>}
      {helper && !error && <div style={{ fontSize: 11.5, color: t.textDim }}>{helper}</div>}
    </div>
  );
}

function TextInput({ theme, value, placeholder, readOnly, monospace, onChange }) {
  const t = T[theme];
  return (
    <input
      type="text"
      value={value}
      placeholder={placeholder}
      readOnly={readOnly}
      onChange={onChange ? (e) => onChange(e.target.value) : undefined}
      style={{
        height: 30,
        padding: "0 10px",
        borderRadius: 4,
        border: `1px solid ${t.borderStrong}`,
        background: readOnly ? t.surface2 : t.surface,
        color: t.text,
        fontSize: 13,
        fontFamily: monospace ? "ui-monospace, 'JetBrains Mono', Menlo, monospace" : "inherit",
        outline: "none",
      }}
    />
  );
}

function Select({ theme, value, options, onChange }) {
  const t = T[theme];
  return (
    <div style={{ position: "relative" }}>
      <select
        value={value}
        onChange={onChange ? (e) => onChange(e.target.value) : undefined}
        style={{
          width: "100%",
          height: 30,
          padding: "0 28px 0 10px",
          borderRadius: 4,
          border: `1px solid ${t.borderStrong}`,
          background: t.surface,
          color: t.text,
          fontSize: 13,
          fontFamily: "inherit",
          appearance: "none",
          outline: "none",
        }}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
      <svg width="10" height="6" viewBox="0 0 10 6" fill="none"
        style={{ position: "absolute", right: 10, top: "50%", transform: "translateY(-50%)", pointerEvents: "none" }}>
        <path d="M1 1L5 5L9 1" stroke={t.textDim} strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </div>
  );
}

function Chip({ theme, children }) {
  const t = T[theme];
  return (
    <span style={{
      display: "inline-block",
      padding: "4px 10px",
      borderRadius: 12,
      background: t.chip,
      color: t.chipText,
      fontSize: 11.5,
      fontWeight: 500,
      border: `1px solid ${t.border}`,
    }}>{children}</span>
  );
}

function Checkbox({ theme, checked, label, helper, onChange }) {
  const t = T[theme];
  return (
    <label style={{ display: "flex", gap: 8, alignItems: "flex-start", cursor: "pointer" }}>
      <span style={{
        width: 16, height: 16, borderRadius: 3, marginTop: 1,
        background: checked ? t.accent : t.surface,
        border: `1px solid ${checked ? t.accent : t.borderStrong}`,
        display: "flex", alignItems: "center", justifyContent: "center",
        flexShrink: 0,
      }}>
        {checked && (
          <svg width="11" height="11" viewBox="0 0 12 12" fill="none">
            <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="white" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        )}
      </span>
      <span>
        <span style={{ fontSize: 13, color: t.text }}>{label}</span>
        {helper && <div style={{ fontSize: 11.5, color: t.textDim, marginTop: 2 }}>{helper}</div>}
      </span>
      <input type="checkbox" checked={checked} onChange={onChange} style={{ display: "none" }} />
    </label>
  );
}

function Divider({ theme }) {
  const t = T[theme];
  return <div style={{ height: 1, background: t.border, margin: "12px 0" }} />;
}

// Animated dotted scan line (simple, Slint-friendly to mirror)
function ScanLine({ theme }) {
  const t = T[theme];
  return (
    <div style={{
      height: 2, position: "relative", overflow: "hidden", borderRadius: 1,
      background: theme === "light" ? "#eceef1" : "#2b2f39",
    }}>
      <div style={{
        position: "absolute", top: 0, left: 0, height: "100%", width: "40%",
        background: `linear-gradient(90deg, transparent, ${t.accent}, transparent)`,
        animation: "tv-scan 1.6s ease-in-out infinite",
      }} />
    </div>
  );
}

// Mini TV illustration (rectangles only — Slint can mirror)
function TvIllustration({ theme, prompt = false, size = 1 }) {
  const t = T[theme];
  const w = 160 * size;
  const h = 110 * size;
  return (
    <div style={{ width: w, height: h, position: "relative" }}>
      {/* bezel */}
      <div style={{
        position: "absolute", inset: 0,
        background: theme === "light" ? "#1f242c" : "#0f1115",
        borderRadius: 8,
        boxShadow: theme === "light" ? "0 4px 12px rgba(0,0,0,.12)" : "0 4px 14px rgba(0,0,0,.4)",
      }} />
      {/* screen */}
      <div style={{
        position: "absolute", inset: 6,
        background: prompt ? "#0a1a2e" : "#0e1422",
        borderRadius: 4,
        overflow: "hidden",
        display: "flex", alignItems: "center", justifyContent: "center",
      }}>
        {prompt ? (
          <div style={{
            background: "rgba(255,255,255,.08)",
            border: `1px solid rgba(255,255,255,.18)`,
            borderRadius: 4,
            padding: "8px 12px",
            color: "#e9edf2",
            fontSize: 9 * size,
            display: "flex", flexDirection: "column", gap: 4,
            minWidth: 90 * size,
          }}>
            <div style={{ fontSize: 8 * size, color: "#a4abb6" }}>tisvlay</div>
            <div style={{ fontSize: 9 * size, fontWeight: 600 }}>Allow this device?</div>
            <div style={{ display: "flex", gap: 4, marginTop: 2 }}>
              <div style={{ background: t.accent, color: "white", fontSize: 7 * size, padding: "2px 6px", borderRadius: 2 }}>Accept</div>
              <div style={{ border: "1px solid #4a4f5b", color: "#cfd4dc", fontSize: 7 * size, padding: "2px 6px", borderRadius: 2 }}>Deny</div>
            </div>
          </div>
        ) : (
          <div style={{ width: 36 * size, height: 24 * size, background: "rgba(255,255,255,.06)", borderRadius: 2 }} />
        )}
      </div>
      {/* stand */}
      <div style={{
        position: "absolute", left: "50%", bottom: -8 * size, width: 36 * size, height: 4 * size,
        background: theme === "light" ? "#1f242c" : "#0f1115",
        transform: "translateX(-50%)", borderRadius: 2,
      }} />
    </div>
  );
}

// Section heading inside body
function SectionHeading({ theme, title, subtitle }) {
  const t = T[theme];
  return (
    <div style={{ marginBottom: 14 }}>
      <div style={{ fontSize: 16, fontWeight: 600, color: t.text }}>{title}</div>
      {subtitle && <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 2 }}>{subtitle}</div>}
    </div>
  );
}

// Body wrapper with consistent padding
function Body({ theme, children, style }) {
  const t = T[theme];
  return (
    <div style={{
      flex: 1,
      padding: "18px 20px",
      overflow: "auto",
      background: t.surface,
      ...style,
    }}>
      {children}
    </div>
  );
}

// Collapsible section
function Collapsible({ theme, title, defaultOpen = false, open: openProp, onToggle, children }) {
  const t = T[theme];
  const [openState, setOpenState] = React.useState(defaultOpen);
  const open = openProp !== undefined ? openProp : openState;
  const toggle = () => {
    if (onToggle) onToggle(!open);
    else setOpenState(!open);
  };
  return (
    <div style={{
      border: `1px solid ${t.border}`,
      borderRadius: 5,
      background: t.surface2,
    }}>
      <button
        onClick={toggle}
        style={{
          width: "100%",
          background: "transparent",
          border: "none",
          padding: "10px 12px",
          display: "flex",
          alignItems: "center",
          gap: 8,
          cursor: "pointer",
          color: t.text,
          fontFamily: "inherit",
          fontSize: 12.5,
          fontWeight: 500,
        }}
      >
        <svg width="9" height="9" viewBox="0 0 10 10" fill="none"
          style={{ transform: open ? "rotate(90deg)" : "rotate(0deg)", transition: "transform .15s" }}>
          <path d="M3 1L7 5L3 9" stroke={t.textDim} strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        <span>{title}</span>
      </button>
      {open && (
        <div style={{ padding: "0 12px 12px", borderTop: `1px solid ${t.border}` }}>
          {children}
        </div>
      )}
    </div>
  );
}

Object.assign(window, {
  T, WindowChrome, Stepper, Footer, Btn, VendorIcon, DiscoveryRow,
  PulseDot, Spinner, FormRow, TextInput, Select, Chip, Checkbox, Divider,
  ScanLine, TvIllustration, SectionHeading, Body, Collapsible, STEPS,
});
