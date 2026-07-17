import { reactive } from 'vue'

export type ToastType = 'success' | 'error' | 'warning' | 'info'

interface ToastMessage {
  id: string
  type: ToastType
  title?: string
  message: string
  duration: number
}

export interface ToastOptions {
  title?: string
  duration?: number
}

const DEFAULT_DURATION = 3000
const messages = reactive<ToastMessage[]>([])
const timers = new Map<string, number>()

function normalizeOptions(options: number | ToastOptions = DEFAULT_DURATION) {
  if (typeof options === 'number') {
    return {
      duration: options,
      title: undefined,
    }
  }

  return {
    duration: options.duration ?? DEFAULT_DURATION,
    title: options.title,
  }
}

function remove(id: string) {
  const timer = timers.get(id)
  if (timer !== undefined) {
    window.clearTimeout(timer)
    timers.delete(id)
  }

  const index = messages.findIndex(message => message.id === id)
  if (index >= 0) {
    messages.splice(index, 1)
  }
}

function show(type: ToastType, message: string, options?: number | ToastOptions) {
  const { duration, title } = normalizeOptions(options)
  const id = `toast_${Date.now()}_${Math.random().toString(16).slice(2)}`

  messages.push({
    id,
    type,
    message,
    duration,
    title,
  })

  if (duration > 0) {
    timers.set(
      id,
      window.setTimeout(() => {
        remove(id)
      }, duration),
    )
  }

  return id
}

export const toast = {
  messages,
  show,
  remove,
  success: (message: string, options?: number | ToastOptions) => show('success', message, options),
  error: (message: string, options?: number | ToastOptions) => show('error', message, options),
  warning: (message: string, options?: number | ToastOptions) => show('warning', message, options),
  info: (message: string, options?: number | ToastOptions) => show('info', message, options),
}
