-- Fixture schema covering every object type Zeble shows.
CREATE SCHEMA IF NOT EXISTS shop;

CREATE TYPE shop.order_status AS ENUM ('pending', 'paid', 'shipped', 'cancelled');
CREATE DOMAIN shop.email AS text CHECK (VALUE ~ '@');
CREATE TYPE shop.address AS (street text, city text, zip text);

CREATE TABLE public.users (
    id          bigserial PRIMARY KEY,
    email       text NOT NULL UNIQUE,
    name        text,
    bio         text,
    settings    jsonb NOT NULL DEFAULT '{}',
    avatar      bytea,
    is_admin    boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE public.users IS 'Application users';

CREATE TABLE shop.products (
    id      serial PRIMARY KEY,
    sku     text NOT NULL,
    title   text NOT NULL,
    price   numeric(10,2) NOT NULL CHECK (price >= 0),
    tags    text[] DEFAULT '{}',
    CONSTRAINT products_sku_key UNIQUE (sku)
);

CREATE TABLE shop.orders (
    id          bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
    status      shop.order_status NOT NULL DEFAULT 'pending',
    total       numeric(12,2),
    ship_to     shop.address,
    placed_at   timestamptz DEFAULT now()
);
CREATE INDEX orders_user_idx ON shop.orders (user_id);
CREATE INDEX orders_status_placed_idx ON shop.orders (status, placed_at DESC);

CREATE TABLE shop.order_items (
    order_id    bigint REFERENCES shop.orders(id),
    product_id  int REFERENCES shop.products(id),
    qty         int NOT NULL DEFAULT 1,
    PRIMARY KEY (order_id, product_id)
);

CREATE TABLE public.event_log (   -- no primary key: paged by ctid
    at      timestamptz DEFAULT now(),
    kind    text,
    payload json
);

CREATE VIEW public.active_admins AS
    SELECT id, email, name FROM public.users WHERE is_admin;

CREATE MATERIALIZED VIEW shop.revenue_by_status AS
    SELECT status, count(*) AS orders, sum(total) AS revenue FROM shop.orders GROUP BY status;

CREATE SEQUENCE public.invoice_no START 1000 INCREMENT 5;

CREATE FUNCTION public.user_order_count(uid bigint) RETURNS bigint
LANGUAGE sql STABLE AS $$ SELECT count(*) FROM shop.orders WHERE user_id = uid $$;

CREATE PROCEDURE shop.cancel_order(oid bigint)
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE shop.orders SET status = 'cancelled' WHERE id = oid;
END $$;

-- Data
INSERT INTO public.users (email, name, bio, settings, avatar, is_admin, created_at)
SELECT 'user' || g || '@example.com',
       CASE WHEN g % 7 = 0 THEN NULL ELSE 'User ' || g END,
       repeat('A rather long biography sentence for user ' || g || '. ', 1 + g % 5),
       jsonb_build_object('theme', CASE WHEN g % 2 = 0 THEN 'dark' ELSE 'light' END, 'n', g),
       CASE WHEN g % 3 = 0 THEN decode(md5(g::text), 'hex') END,
       g % 50 = 0,
       now() - (g || ' hours')::interval
FROM generate_series(1, 2345) g;

INSERT INTO shop.products (sku, title, price, tags)
SELECT 'SKU-' || g, 'Product ' || g, (g * 3.17)::numeric(10,2), ARRAY['tag' || (g % 4), 'x']
FROM generate_series(1, 120) g;

INSERT INTO shop.orders (user_id, status, total, ship_to, placed_at)
SELECT 1 + (g * 13) % 2345,
       (ARRAY['pending','paid','shipped','cancelled']::shop.order_status[])[1 + g % 4],
       (g % 500) + 0.99,
       ROW('Street ' || g, 'City ' || (g % 30), lpad((g % 99999)::text, 5, '0'))::shop.address,
       now() - (g || ' minutes')::interval
FROM generate_series(1, 5000) g;

INSERT INTO shop.order_items SELECT o, 1 + o % 120, 1 + o % 3 FROM generate_series(1, 5000) o;
INSERT INTO public.event_log (kind, payload)
SELECT 'evt' || (g % 5), json_build_object('g', g) FROM generate_series(1, 300) g;
REFRESH MATERIALIZED VIEW shop.revenue_by_status;
