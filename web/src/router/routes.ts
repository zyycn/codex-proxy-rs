import type { RouteRecordRaw } from 'vue-router'

import AdminLayout from '@/layout/index.vue'
import DashboardView from '@/views/dashboard/DashboardView.vue'
import LoginView from '@/views/login/LoginView.vue'

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
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
