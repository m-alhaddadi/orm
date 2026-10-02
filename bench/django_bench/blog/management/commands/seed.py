import random
from datetime import datetime, timedelta, timezone

from django.core.management.base import BaseCommand
from django.db import connection, transaction

from blog.models import Author, Comment, Post

N_AUTHORS = 50
N_POSTS = 1000
N_COMMENTS = 2000


class Command(BaseCommand):
    help = "Reset tables and seed deterministic benchmark data."

    def handle(self, *args, **options):
        rng = random.Random(42)
        base = datetime(2026, 1, 1, tzinfo=timezone.utc)
        with transaction.atomic():
            with connection.cursor() as cur:
                cur.execute(
                    "TRUNCATE blog_comment, blog_post, blog_author RESTART IDENTITY CASCADE"
                )
            authors = Author.objects.bulk_create(
                Author(
                    name=f"Author {i}",
                    email=f"author{i}@example.com",
                    created_at=base + timedelta(minutes=i),
                )
                for i in range(N_AUTHORS)
            )
            posts = Post.objects.bulk_create(
                Post(
                    author=rng.choice(authors),
                    title=f"Post title number {i}",
                    body="Lorem ipsum dolor sit amet, consectetur adipiscing elit. " * 4,
                    views=rng.randint(0, 100_000),
                    published=rng.random() < 0.7,
                    created_at=base + timedelta(hours=i),
                )
                for i in range(N_POSTS)
            )
            Comment.objects.bulk_create(
                Comment(
                    post=rng.choice(posts),
                    author_name=f"Commenter {i}",
                    body="Nice post! " * 3,
                    created_at=base + timedelta(hours=i, minutes=5),
                )
                for i in range(N_COMMENTS)
            )
        with connection.cursor() as cur:
            cur.execute("VACUUM ANALYZE blog_author")
            cur.execute("VACUUM ANALYZE blog_post")
            cur.execute("VACUUM ANALYZE blog_comment")
        self.stdout.write(
            f"seeded {N_AUTHORS} authors, {N_POSTS} posts, {N_COMMENTS} comments"
        )
