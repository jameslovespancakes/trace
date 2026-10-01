"""Fixture (bridges gate, django family): a URL pattern."""
from django.urls import path

from . import django_views as views

urlpatterns = [
    path("django/items/<int:pk>/", views.item),
]
