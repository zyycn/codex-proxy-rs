import { defineStore } from 'pinia'
import { ref } from 'vue'

export interface Toast {
  id: string
  type: 'success' | 'error' | 'warning' | 'info'
  message: string
  duration?: number
}

export const useToastStore = defineStore('toast', () => {
  const toasts = ref<Toast[]>([])

  function show(type: Toast['type'], message: string, duration = 3000) {
    const id = `toast_${Date.now()}_${Math.random()}`
    const toast: Toast = { id, type, message, duration }

    toasts.value.push(toast)

    if (duration > 0) {
      setTimeout(() => {
        remove(id)
      }, duration)
    }

    return id
  }

  function success(message: string, duration?: number) {
    return show('success', message, duration)
  }

  function error(message: string, duration?: number) {
    return show('error', message, duration)
  }

  function warning(message: string, duration?: number) {
    return show('warning', message, duration)
  }

  function info(message: string, duration?: number) {
    return show('info', message, duration)
  }

  function remove(id: string) {
    const index = toasts.value.findIndex(t => t.id === id)
    if (index > -1) {
      toasts.value.splice(index, 1)
    }
  }

  return {
    toasts,
    show,
    success,
    error,
    warning,
    info,
    remove,
  }
})
