import { themes as prismThemes } from "prism-react-renderer";
import type { Config } from "@docusaurus/types";
import type * as Preset from "@docusaurus/preset-classic";

const config: Config = {
  title: "awob",
  tagline: "Another Wayland Overlay Bar",
  favicon: "img/favicon.ico",

  // Deployed to GitHub Pages at https://jmylchreest.github.io/awob/.
  url: "https://jmylchreest.github.io",
  baseUrl: "/awob/",

  organizationName: "jmylchreest",
  projectName: "awob",
  deploymentBranch: "gh-pages",
  trailingSlash: false,

  onBrokenLinks: "throw",
  onBrokenMarkdownLinks: "warn",

  i18n: {
    defaultLocale: "en",
    locales: ["en"],
  },

  presets: [
    [
      "classic",
      {
        docs: {
          sidebarPath: "./sidebars.ts",
          // Edit-on-GitHub link points readers at the source.
          editUrl: "https://github.com/jmylchreest/awob/edit/main/docs/",
          routeBasePath: "/",
        },
        blog: false,
        theme: {
          customCss: "./src/css/custom.css",
        },
      } satisfies Preset.Options,
    ],
  ],

  themes: [
    [
      // @easyops-cn/docusaurus-search-local supports Docusaurus 3.x
      // (the older @cmfcmf/docusaurus-search-local pins to v2). Local
      // search, no external service required, ships with the static
      // site and works on GH Pages out of the box.
      "@easyops-cn/docusaurus-search-local",
      {
        hashed: true,
        indexBlog: false,
        docsRouteBasePath: "/",
      },
    ],
  ],

  themeConfig: {
    image: "img/social-card.png",
    navbar: {
      title: "awob",
      logo: {
        alt: "awob logo",
        src: "img/logo.svg",
      },
      items: [
        {
          type: "docSidebar",
          sidebarId: "main",
          position: "left",
          label: "Docs",
        },
        {
          href: "https://github.com/jmylchreest/awob",
          label: "GitHub",
          position: "right",
        },
      ],
    },
    footer: {
      style: "dark",
      links: [
        {
          title: "Docs",
          items: [
            { label: "Getting Started", to: "/getting-started/install" },
            { label: "Usage", to: "/usage" },
            { label: "Themes", to: "/themes" },
            { label: "Protocol", to: "/protocol" },
          ],
        },
        {
          title: "Project",
          items: [
            { label: "GitHub", href: "https://github.com/jmylchreest/awob" },
            { label: "Issues", href: "https://github.com/jmylchreest/awob/issues" },
            { label: "Releases", href: "https://github.com/jmylchreest/awob/releases" },
          ],
        },
        {
          title: "Related",
          items: [
            { label: "wob (the original)", href: "https://github.com/francma/wob" },
            { label: "tinct (theme generator)", href: "https://github.com/jmylchreest/tinct" },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} John Mylchreest. MIT licensed.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ["bash", "toml", "rust", "json"],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
