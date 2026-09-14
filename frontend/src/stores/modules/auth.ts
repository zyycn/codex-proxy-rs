import type { AuthSession } from '@/api'

import { defineStore } from 'pinia'
import { computed, shallowRef } from 'vue'

import { login as apiLogin, logout as apiLogout, getAuthStatus } from '@/api'
import { resetUnauthorizedHandling } from '@/api/request'

export const useAuthStore = defineStore('auth', () => {
  const session = shallowRef<AuthSession | null>(null)
  const isAuthenticated = computed(() => session.value !== null)
  const sessionChecked = shallowRef(false)
  const loading = shallowRef(false)
  let revision = 0
  let pendingCheck: Promise<boolean> | undefined

  function checkAuth(): Promise<boolean> {
    if (pendingCheck)
      return pendingCheck
    const currentRevision = revision
    const check = getAuthStatus().then((status) => {
      // 登录或退出之后到达的旧状态响应，不覆盖新会话。
      if (currentRevision === revision) {
        session.value = status.session
        sessionChecked.value = true
        if (status.authenticated)
          resetUnauthorizedHandling()
      }
      return isAuthenticated.value
    }).finally(() => {
      if (pendingCheck === check)
        pendingCheck = undefined
    })
    pendingCheck = check
    return check
  }

  async function login(payload: Parameters<typeof apiLogin>[0]) {
    if (loading.value)
      return null
    revision += 1
    pendingCheck = undefined
    loading.value = true
    try {
      const result = await apiLogin(payload)
      revision += 1
      pendingCheck = undefined
      session.value = result
      sessionChecked.value = true
      resetUnauthorizedHandling()
      return result
    }
    catch {
      return null
    }
    finally {
      loading.value = false
    }
  }

  async function logout() {
    if (loading.value)
      return false
    loading.value = true
    revision += 1
    pendingCheck = undefined
    try {
      await apiLogout()
      invalidateSession()
      return true
    }
    catch {
      // 只有服务端确认撤销后才退出，避免刷新又恢复一个未撤销的会话。
      return false
    }
    finally {
      loading.value = false
    }
  }

  function invalidateSession() {
    revision += 1
    pendingCheck = undefined
    session.value = null
    sessionChecked.value = true
    resetUnauthorizedHandling()
  }

  return { session, isAuthenticated, sessionChecked, loading, checkAuth, login, logout, invalidateSession }
})
