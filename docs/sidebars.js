// Sidebar layout: outcome-first. Most readers want either "how do I
// install + use this" (Getting Started → Usage) or "how do I theme it"
// (Themes), with the protocol page reserved for listener / SDK
// authors.
const sidebars = {
    main: [
        "intro",
        {
            type: "category",
            label: "Getting Started",
            collapsed: false,
            items: [
                "getting-started/install",
                "getting-started/quickstart",
                "getting-started/hyprland",
                "getting-started/gnome",
                "getting-started/sway",
                "getting-started/migrating-from-wob",
            ],
        },
        "usage",
        "listeners",
        "themes",
        "protocol",
    ],
};
export default sidebars;
