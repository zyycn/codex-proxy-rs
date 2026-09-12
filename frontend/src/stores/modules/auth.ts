import { defineStore } from 'pinia'
import { ref } from 'vue'

import { login as apiLogin, logout as apiLogout, getAuthStatus } from '@/api'
import { resetUnauthorizedHandling } from '@/api/request'

export const useAuthStore = defineStore('auth', () => {
  const isAuthenticated = ref(false)
  const sessionChecked = ref(false)
  const loading = ref(false)

  async function checkAuth() {
    try {
      const status = await getAuthStatus({ silent: true })
      isAuthenticated.value = status.authenticated
      if (status.authenticated)
        resetUnauthorizedHandling()
      return status.authenticated
    }
    catch {
      isAuthenticated.value = false
      return false
    }
    finally {
      sessionChecked.value = true
    }
  }

  async function login(payload: Parameters<typeof apiLogin>[0]) {
    try {
      loading.value = true
      await apiLogin(payload)

      isAuthenticated.value = true
      sessionChecked.value = true
      resetUnauthorizedHandling()

      return true
    }
    catch {
      isAuthenticated.value = false
      return false
    }
    finally {
      loading.value = false
    }
  }

  async function logout() {
    try {
      await apiLogout({ silent: true })
    }
    catch {
      // 忽略登出错误
    }
    finally {
      isAuthenticated.value = false
      sessionChecked.value = true
    }
  }

  function invalidateSession() {
    isAuthenticated.value = false
    sessionChecked.value = true
    loading.value = false
  }

  return {
    isAuthenticated,
    sessionChecked,
    loading,
    checkAuth,
    login,
    logout,
    invalidateSession,
  }
})
