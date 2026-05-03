import Link from "@docusaurus/Link";
import useDocusaurusContext from "@docusaurus/useDocusaurusContext";
import Layout from "@theme/Layout";

export default function Home(): JSX.Element {
  const { siteConfig } = useDocusaurusContext();
  return (
    <Layout
      title={siteConfig.title}
      description={siteConfig.tagline}
    >
      <main style={{ padding: "4rem 1rem", textAlign: "center" }}>
        <h1 style={{ fontSize: "3rem", marginBottom: "0.5rem" }}>
          {siteConfig.title}
        </h1>
        <p style={{ fontSize: "1.25rem", marginBottom: "2rem", opacity: 0.85 }}>
          {siteConfig.tagline}
        </p>

        <pre
          style={{
            display: "inline-block",
            textAlign: "left",
            padding: "1rem 2rem",
            margin: "0 auto 2rem",
            fontFamily: "monospace",
            fontSize: "0.95rem",
          }}
        >
{`┌────────────────────────────────────────────────────┐
│  VOLUME                                            │
│  █ █ █ █ █ █ █ █ █ █ █ █ █ █ ░ ░ ░ ░ ░ ░          │
└────────────────────────────────────────────────────┘`}
        </pre>

        <div style={{ display: "flex", gap: "1rem", justifyContent: "center", flexWrap: "wrap" }}>
          <Link
            className="button button--primary button--lg"
            to="/getting-started/install"
          >
            Install
          </Link>
          <Link
            className="button button--secondary button--lg"
            to="/intro"
          >
            What is awob?
          </Link>
          <Link
            className="button button--secondary button--lg"
            to="https://github.com/jmylchreest/awob"
          >
            GitHub
          </Link>
        </div>
      </main>
    </Layout>
  );
}
