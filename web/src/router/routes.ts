import type { RouteRecordRaw } from 'vue-router'

import AdminLayout from '@/layout/index.vue'
import AccountsView from '@/views/accounts/index.vue'
import ApiKeysView from '@/views/api-keys/index.vue'
import DashboardView from '@/views/dashboard/index.vue'
import LoginView from '@/views/login/index.vue'
import LogsView from '@/views/logs/index.vue'
import SettingsView from '@/views/settings/index.vue'

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: LoginView,
  },
  {
    path: '/',
    component: AdminLayout,
    children: [
      {
        path: '',
        name: 'dashboard',
        component: DashboardView,
      },
      {
        path: 'accounts',
        name: 'accounts',
        component: AccountsView,
      },
      {
        path: 'api-keys',
        name: 'api-keys',
        component: ApiKeysView,
      },
      {
        path: 'logs',
        name: 'logs',
        component: LogsView,
      },
      {
        path: 'settings',
        name: 'settings',
        component: SettingsView,
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
