import Link from "@docusaurus/Link";
import useDocusaurusContext from "@docusaurus/useDocusaurusContext";
import Layout from "@theme/Layout";
import styles from "./index.module.css";
function Hero() {
    const { siteConfig } = useDocusaurusContext();
    return (<section className={styles.heroSection}>
      <h1 className={styles.wordmark}>
        a<span className={styles.accent}>wob</span>
      </h1>
      <p className={styles.tagline}>{siteConfig.tagline}</p>
      <p className={styles.expanded}>
        <strong>a</strong>nother <strong>w</strong>ayland{" "}
        <strong>o</strong>verlay <strong>b</strong>ar
      </p>

      <div className={styles.heroButtons}>
        <Link className="button button--primary button--lg" to="/getting-started/install">
          Install
        </Link>
        <Link className="button button--secondary button--lg" to="/intro">
          What is awob?
        </Link>
        <Link className="button button--secondary button--lg" to="https://github.com/jmylchreest/awob">
          GitHub
        </Link>
      </div>

      <div className={styles.terminal}>
        <div className={styles.terminalBar}>
          <span className={styles.terminalDot} style={{ background: "#ff5f56" }}/>
          <span className={styles.terminalDot} style={{ background: "#ffbd2e" }}/>
          <span className={styles.terminalDot} style={{ background: "#27c93f" }}/>
          <span className={styles.terminalTitle}>awob — preview</span>
        </div>
        <pre className={styles.terminalBody}>
    {`${"$"} `}<span className={styles.terminalPrompt}>awob send --preempt --icon audio-volume-high volume 0.7 1.0</span>{`

  ┌──────────────────────────────────────────────────┐
  │  VOLUME                                          │
  │  █ █ █ █ █ █ █ █ █ █ █ █ █ █ ░ ░ ░ ░ ░ ░         │
  └──────────────────────────────────────────────────┘
`}
        </pre>
      </div>
    </section>);
}
function Feature({ title, body }) {
    return (<div className={styles.feature}>
      <h3>{title}</h3>
      <p>{body}</p>
    </div>);
}
function Features() {
    return (<section className={styles.features}>
      <h2 className={styles.featuresHeader}>Why awob?</h2>
      <div className={styles.featureGrid}>
        <Feature title="Drop-in for wob" body="Same FIFO format, same scripts. Pixel-faithful 'wob' theme bundled. Migrate by editing one config file."/>
        <Feature title="Theming as data" body="KDL scene files describe a small element tree (rect, text, image, bar) with bindings, expressions, and palette imports. Hot-reloaded on save."/>
        <Feature title="Typed IPC" body="JSON-line protocol over a Unix socket. Send (event, value, source, style, icon, …) in one structured payload — no positional parsing."/>
        <Feature title="Listener ecosystem" body="PipeWire, UPower, sysfs backlight, keyboard backlight — auto-discovered and supervised. Bring your own via the same protocol."/>
        <Feature title="Preempt-aware" body="User-driven sends hot-swap; ambient updates queue politely. History keyed by (source, event) so distinct metrics never cross-contaminate."/>
        <Feature title="Wayland-first" body="wlr-layer-shell-v1, tiny-skia, cosmic-text, resvg. No X11, no GTK, no Qt. Works on Hyprland, Sway, KDE Plasma, river, Wayfire — anywhere wlr-layer-shell exists."/>
      </div>
    </section>);
}
export default function Home() {
    const { siteConfig } = useDocusaurusContext();
    return (<Layout title={siteConfig.title} description={siteConfig.tagline}>
      <Hero />
      <Features />
    </Layout>);
}
