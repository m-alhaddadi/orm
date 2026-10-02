"""SQLAlchemy 2.0 mapping of the Django-created tables (blog_author, blog_post)."""

from datetime import datetime

from sqlalchemy import BigInteger, DateTime, ForeignKey, String, Text
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship


class Base(DeclarativeBase):
    pass


class Author(Base):
    __tablename__ = "blog_author"

    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    name: Mapped[str] = mapped_column(String(100))
    email: Mapped[str] = mapped_column(String(254), unique=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))


class Post(Base):
    __tablename__ = "blog_post"

    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    author_id: Mapped[int] = mapped_column(ForeignKey("blog_author.id"))
    title: Mapped[str] = mapped_column(String(200))
    body: Mapped[str] = mapped_column(Text)
    views: Mapped[int]
    published: Mapped[bool]
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))

    author: Mapped[Author] = relationship(lazy="raise")
