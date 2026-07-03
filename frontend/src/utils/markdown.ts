import DOMPurify from 'dompurify'
import { Marked, Renderer } from 'marked'

const renderer = new Renderer()
const defaultLinkRenderer = renderer.link.bind(renderer)

renderer.link = (token) => {
  const html = defaultLinkRenderer(token)
  return html.replace('<a ', '<a target="_blank" rel="noreferrer" ')
}

const marked = new Marked({
  async: false,
  breaks: false,
  gfm: true,
  renderer,
})

export function renderMarkdown(source?: string | null) {
  const value = source?.trim()
  if (!value) return ''

  return DOMPurify.sanitize(marked.parse(value, { async: false }), {
    ADD_ATTR: ['target', 'rel'],
  })
}
