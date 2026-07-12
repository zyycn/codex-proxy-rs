import { defineStore } from 'pinia'
import { ref } from 'vue'

import { getAuthStatus, login as apiLogin, logout as apiLogout } from '@/api'
import { resetUnauthorizedHandling } from '@/api/request'

export const useAuthStore = defineStore('auth', () => {
  const isAuthenticated = ref(false)
  const sessionChecked = ref(false)
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function checkAuth() {
    try {
      const status = await getAuthStatus()
      isAuthenticated.value = status.authenticated
      if (status.authenticated) resetUnauthorizedHandling()
      return status.authenticated
    } catch (err: any) {
      isAuthenticated.value = false
      return false
    } finally {
      sessionChecked.value = true
    }
  }

  async function login(payload: any) {
    try {
      loading.value = true
      error.value = null
      await apiLogin(payload)

      isAuthenticated.value = true
      sessionChecked.value = true
      resetUnauthorizedHandling()

      return true
    } catch (err: any) {
      error.value = err.message || '登录失败'
      isAuthenticated.value = false
      return false
    } finally {
      loading.value = false
    }
  }

  async function logout() {
    try {
      await apiLogout()
    } catch (err) {
      // 忽略登出错误
    } finally {
      isAuthenticated.value = false
      sessionChecked.value = true
    }
  }

  function invalidateSession() {
    isAuthenticated.value = false
    sessionChecked.value = true
    loading.value = false
    error.value = null
  }

  return {
    isAuthenticated,
    sessionChecked,
    loading,
    error,
    checkAuth,
    login,
    logout,
    invalidateSession,
  }
})
