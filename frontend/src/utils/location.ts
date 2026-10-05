import type { RequestLocation } from '@/api'

export function normalizeRequestLocation(location: RequestLocation): RequestLocation {
  return {
    country: location.country.trim().toUpperCase(),
    region: location.region.trim(),
    city: location.city.trim(),
    timezone: location.timezone.trim(),
  }
}

export function requestLocationError(location: RequestLocation): string {
  if (!/^[A-Z]{2}$/.test(location.country) || !location.region || !location.city || !location.timezone)
    return '请填写两位国家代码、地区、城市和 IANA 时区'
  return ''
}
