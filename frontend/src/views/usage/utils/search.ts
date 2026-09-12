const CLIENT_API_KEY_VISIBLE_PREFIX_LENGTH = 10

export function usageSearchParam(value: string) {
  const search = value.trim()
  if (!search)
    return undefined
  if (search.startsWith('sk_'))
    return search.slice(0, CLIENT_API_KEY_VISIBLE_PREFIX_LENGTH)
  return search
}
