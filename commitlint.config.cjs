// commitlint.config.cjs
// Enforces Conventional Commits (https://www.conventionalcommits.org/) so
// that git-cliff can group commits into CHANGELOG.md sections automatically.
//
// Header format:
//   <type>(<scope>)<!>: <subject>
//
//   type:    feat | fix | docs | refactor | perf | test | build | ci | chore | style | revert
//   scope:   optional, kebab-case (e.g. chat, vision, ipc, fs, adr, deps)
//   !:       optional "BREAKING CHANGE" marker (must also be in body/footer)
//   subject: imperative, no trailing dot, ≤72 chars
//
// Body / footer free-form. Recognized footers:
//   BREAKING CHANGE: <description>
//   Refs: #123
//   Closes: #123
//   ADR: docs/adr/0007-foo.md

module.exports = {
  extends: ['@commitlint/config-conventional'],
  rules: {
    // Type enum — keep this list in sync with .gitcliff.toml [changelog] sections.
    'type-enum': [
      2,
      'always',
      [
        'feat',
        'fix',
        'docs',
        'refactor',
        'perf',
        'test',
        'build',
        'ci',
        'chore',
        'style',
        'revert',
      ],
    ],
    // Subject formatting.
    'subject-case': [2, 'always', ['lower-case', 'sentence-case']],
    'subject-empty': [2, 'never'],
    'subject-full-stop': [2, 'never', '.'],
    'header-max-length': [2, 'always', 100],
    // Type must be present.
    'type-empty': [2, 'never'],
    // Body wrapping.
    'body-leading-blank': [2, 'always'],
    'body-max-line-length': [2, 'always', 100],
    // Footer.
    'footer-leading-blank': [2, 'always'],
    // Conventional Commits spec — disallow merge / revert in subject (use type=revert).
    'type-case': [2, 'always', 'lower-case'],
  },
  // Allow common scopes; not enforced — anything kebab-case works.
  ignores: [
    // Allow release commits produced by git-cliff / release-please.
    (commit) => /^chore\(release\):/i.test(commit) ||
      /^docs\(changelog\):/i.test(commit),
  ],
  helpUrl:
    'https://github.com/conventional-changelog/commitlint/#what-is-commitlint',
};
