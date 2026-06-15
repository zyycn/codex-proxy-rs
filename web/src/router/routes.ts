import type { RouteRecordRaw } from 'vue-router'

import AdminLayout from '@/layout/index.vue'
import DashboardView from '@/views/dashboard/DashboardView.vue'

export const routes: RouteRecordRaw[] = [
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
