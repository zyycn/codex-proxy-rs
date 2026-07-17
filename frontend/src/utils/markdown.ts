import DOMPurify from 'dompurify'
import { Marked } from 'marked'

const marked = new Marked({
  async: false,
  breaks: false,
  gfm: true,
})

export function renderMarkdown(source?: string | null) {
  const value = source?.trim()
  if (!value)
    return ''

  const html = marked
    .parse(value, { async: false })
    .replaceAll('<a ', '<a target="_blank" rel="noreferrer" ')

  return DOMPurify.sanitize(html, {
    ADD_ATTR: ['target', 'rel'],
  })
}
