import type { Plugin } from 'vite'

const banner = `
 ██████╗ ██████╗  ██████╗
██╔════╝ ██╔══██╗ ██╔══██╗
██║      ██████╔╝ ██████╔╝
██║      ██╔═══╝  ██╔═██╗
╚██████╗ ██║      ██║  ██╗
 ╚═════╝ ╚═╝      ╚═╝  ╚═╝

https://github.com/zyycn/codex-proxy-rs`

export function consoleBanner(): Plugin {
  return {
    name: 'cpr-console-banner',
    transformIndexHtml: {
      order: 'pre',
      handler: () => [
        {
          tag: 'script',
          attrs: { type: 'module' },
          children: `console.info(${JSON.stringify(banner)})`,
        },
      ],
    },
  }
}
