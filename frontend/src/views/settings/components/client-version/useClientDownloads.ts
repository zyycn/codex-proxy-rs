import type { CodexDesktopWindowsDownloads } from '@/api'
import { shallowRef } from 'vue'

import { getCodexDesktopWindowsDownloads } from '@/api'

export function useClientDownloads() {
  const open = shallowRef(false)
  const loading = shallowRef(false)
  const downloads = shallowRef<CodexDesktopWindowsDownloads | null>(null)

  async function load(refresh = false): Promise<void> {
    if (loading.value)
      return
    loading.value = true
    try {
      downloads.value = await getCodexDesktopWindowsDownloads({ refresh })
    }
    catch {
      // 请求层统一提示失败，保留上次可用的下载链接
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
    downloads,
    load,
    showGuide,
  }
}
