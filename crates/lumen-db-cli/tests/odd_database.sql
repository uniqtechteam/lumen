-- A database written by hand, the way an older app grows: odd names, a self-relation, two
-- relations between the same tables, a composite foreign key, join tables, a table without a
-- key, a view, types the client cannot read, partial and expression indexes, a check constraint.
CREATE TYPE "order status" AS ENUM ('new', 'in-progress', 'done', '2nd-try');

CREATE TABLE "customer accounts" (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "E-mail" varchar(320) NOT NULL UNIQUE,
    display_name text,
    referred_by bigint REFERENCES "customer accounts" (id) ON DELETE SET NULL,
    created_at timestamptz(6) NOT NULL DEFAULT now(),
    tags text[] NOT NULL DEFAULT '{}',
    search tsvector,
    "1st_seen" date
);
CREATE UNIQUE INDEX customer_email_lower ON "customer accounts" (lower("E-mail"));

CREATE TABLE orders (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    buyer_id bigint NOT NULL REFERENCES "customer accounts" (id),
    seller_id bigint NOT NULL REFERENCES "customer accounts" (id) ON DELETE CASCADE,
    status "order status" NOT NULL DEFAULT 'in-progress',
    total numeric(12, 2) NOT NULL CONSTRAINT total_not_negative CHECK (total >= 0),
    placed_at timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at timestamp,
    wait interval
);
CREATE INDEX orders_live ON orders (placed_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX orders_status_hash ON orders USING hash (status);

CREATE TABLE order_lines (
    order_id uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    sku text NOT NULL,
    qty smallint NOT NULL DEFAULT 1,
    PRIMARY KEY (order_id, line_no)
);

CREATE TABLE shipments (
    id serial PRIMARY KEY,
    order_id uuid NOT NULL,
    line_no integer NOT NULL,
    CONSTRAINT shipment_line FOREIGN KEY (order_id, line_no) REFERENCES order_lines (order_id, line_no)
);

CREATE TABLE string (id integer PRIMARY KEY, value text);

CREATE TABLE "Tag" (id serial PRIMARY KEY, name text NOT NULL UNIQUE);

CREATE TABLE "_OrdersToTag" (
    "A" uuid NOT NULL,
    "B" integer NOT NULL,
    CONSTRAINT "_OrdersToTag_AB_pkey" PRIMARY KEY ("A", "B"),
    CONSTRAINT "_OrdersToTag_A_fkey" FOREIGN KEY ("A") REFERENCES orders (id) ON DELETE CASCADE ON UPDATE CASCADE,
    CONSTRAINT "_OrdersToTag_B_fkey" FOREIGN KEY ("B") REFERENCES "Tag" (id) ON DELETE CASCADE ON UPDATE CASCADE
);
CREATE INDEX "_OrdersToTag_B_index" ON "_OrdersToTag" ("B");

CREATE TABLE favourites (
    customer_id bigint NOT NULL REFERENCES "customer accounts" (id),
    tag_id integer NOT NULL REFERENCES "Tag" (id),
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (customer_id, tag_id)
);

CREATE TABLE audit_log (at timestamptz NOT NULL DEFAULT now(), message text);

CREATE VIEW order_totals AS SELECT buyer_id, sum(total) AS total FROM orders GROUP BY buyer_id;
