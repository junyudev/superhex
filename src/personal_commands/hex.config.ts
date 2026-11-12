import { defineHexConfig } from "@hex/commands"

// Superhex runs this file itself and reloads it when you save. It is plain
// TypeScript or JavaScript with no npm packages: transformations return text.
// fetch and node:child_process are there once you allow them in Superhex; see
// https://anoma.ly/hex/docs/transforms
export default defineHexConfig({
  // Transformations appear as optional final steps in every dictation mode.
  transformations: {
    "trim-whitespace": {
      name: "Trim whitespace",
      description: "Remove leading and trailing whitespace",
      transform: (text) => text.trim(),
    },
  },
})
