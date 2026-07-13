import type { ThemeRegistration } from "shiki";

/**
 * "TokyoWhale" — the user's personal Zed theme, ported to a Shiki
 * {@link ThemeRegistration}. The palette below is authoritative (Zed token →
 * color); the TextMate scope mapping is hand-authored to cover the languages
 * this app renders well (TS/TSX, Rust, JSON, CSS, PHP, Python).
 *
 * `tokenColors` are ordered general → specific so that a more specific scope
 * (e.g. `entity.name.function`) overrides a broad one (e.g. `variable`) when
 * both match a token.
 */
export const tokyowhale: ThemeRegistration = {
  name: "tokyowhale",
  type: "dark",
  colors: {
    "editor.background": "#0a0a0d",
    "editor.foreground": "#a9b1d6",
  },
  // `fg`/`bg` mirror the editor colors so codeToTokensBase resolves a sane
  // default foreground/background even for unscoped tokens.
  fg: "#a9b1d6",
  bg: "#0a0a0d",
  tokenColors: [
    // --- General fallbacks (broadest scopes first) ---
    {
      // variable: identifiers, parameters, read/write locals
      scope: [
        "variable",
        "variable.other.readwrite",
        "variable.parameter",
        "meta.definition.variable.name",
      ],
      settings: { foreground: "#fd8a78" },
    },
    {
      // punctuation / preprocessor / control directives
      scope: [
        "punctuation",
        "punctuation.separator",
        "punctuation.terminator",
        "punctuation.section",
        "meta.brace",
        "meta.preprocessor",
        "keyword.control.directive",
        "keyword.control.import",
        "keyword.control.at-rule",
      ],
      settings: { foreground: "#cfd2d5" },
    },

    // --- Types & classes ---
    {
      // type, hint
      scope: [
        "entity.name.type",
        "entity.name.class",
        "entity.other.inherited-class",
        "support.type",
        "support.class",
        "storage.type.class",
        "storage.type.interface",
        "meta.type.annotation",
      ],
      settings: { foreground: "#a8c1ca" },
    },

    // --- Constructors / enums / namespaces / variants ---
    {
      scope: [
        "entity.name.namespace",
        "entity.name.type.enum",
        "entity.name.type.namespace",
        "entity.name.type.module",
        "entity.name.function.constructor",
        "variable.other.enummember",
        "meta.module-reference",
      ],
      settings: { foreground: "#ecbc61" },
    },

    // --- Keywords / storage / attributes / labels / selectors ---
    {
      scope: [
        "keyword",
        "storage.type",
        "storage.modifier",
        "keyword.other",
        "entity.other.attribute-name",
        "entity.name.label",
        "entity.name.tag.css",
        "entity.other.attribute-name.class.css",
        "entity.other.attribute-name.id.css",
        "markup.italic",
      ],
      settings: { foreground: "#C792EA" },
    },

    // --- Operators / escapes / regex ---
    {
      scope: [
        "keyword.operator",
        "constant.character.escape",
        "string.regexp",
        "constant.other.character-class.regexp",
        "keyword.operator.arithmetic",
        "keyword.operator.logical",
      ],
      settings: { foreground: "#89ddff" },
    },

    // --- Constants / numbers / booleans ---
    {
      scope: [
        "constant.language",
        "constant.numeric",
        "variable.other.constant",
        "constant.language.boolean",
        "support.constant",
        "keyword.other.unit",
      ],
      settings: { foreground: "#fd6161" },
    },

    // --- Properties ---
    {
      scope: [
        "variable.other.property",
        "support.type.property-name",
        "meta.object-literal.key",
        "variable.other.object.property",
        "support.type.property-name.css",
        "support.type.property-name.json",
      ],
      settings: { foreground: "#B2CCD6" },
    },

    // --- Functions / builtins / links ---
    {
      scope: [
        "entity.name.function",
        "support.function",
        "meta.function-call entity.name.function",
        "meta.function-call.generic",
        "markup.underline.link",
        "string.other.link",
      ],
      settings: { foreground: "#39ccda" },
    },

    // --- Strings ---
    {
      scope: [
        "string",
        "string.quoted",
        "string.template",
        "string.other",
        "constant.other.symbol",
      ],
      settings: { foreground: "#9ce88d" },
    },

    // --- Tags / language variables ---
    {
      scope: [
        "entity.name.tag",
        "variable.language",
        "variable.language.this",
        "variable.language.self",
        "support.type.object.dom",
      ],
      settings: { foreground: "#f07178" },
    },

    // --- Comments (specific, so they win over embedded scopes) ---
    {
      scope: ["comment", "punctuation.definition.comment", "comment.block.documentation"],
      settings: { foreground: "#708090", fontStyle: "italic" },
    },

    // --- Markup emphasis ---
    {
      scope: ["markup.heading", "markup.heading entity.name"],
      settings: { foreground: "#39ccda", fontStyle: "bold" },
    },
    {
      scope: ["markup.bold", "markup.bold.markdown"],
      settings: { foreground: "#C792EA", fontStyle: "bold" },
    },
  ],
};

export default tokyowhale;
