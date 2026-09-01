import type { CodexDesktopWindowsDownloads } from '@/api'
import { shallowRef } from 'vue'

import { getCodexDesktopWindowsDownloads } from '@/api'
import { errorMessage } from '@/utils/async'

export function useClientDownloads() {
  const open = shallowRef(false)
  const loading = shallowRef(false)
  const error = shallowRef('')
  const downloads = shallowRef<CodexDesktopWindowsDownloads | null>(null)

  async function load(refresh = false): Promise<void> {
    if (loading.value)
      return
    loading.value = true
    error.value = ''
    try {
      downloads.value = await getCodexDesktopWindowsDownloads(refresh)
    }
    catch (cause: unknown) {
      error.value = errorMessage(cause, 'Windows 离线安装包加载失败')
    }
    finally {
      loading.value = false
    }
  }

  function showGuide() {
    open.value = true
    if (!downloads.value)
      void load()
  }

  return {
    open,
    loading,
    error,
    downloads,
    load,
    showGuide,
  }
}
